//! Read-only reports built from the store, one per UI page.
//!
//! These live here rather than in the app so the numbers can be tested
//! without a window, and so the visual layer stays free to change without
//! touching anything that computes. Each returns display lines: the app
//! renders them and nothing else.

use std::collections::BTreeMap;

use chrono::Utc;
use ptt_market_book::{
    DataVisibility, EvaluatedQuoteEdge, FreshnessPolicy, FreshnessStatus, QuoteSelectionPolicy,
    QuoteSelectionStrategy, build_coherent_current_book, select_quote_edges,
};
use ptt_settings::{MarketTuning, UiLanguage};
use ptt_strategy::{
    BucketSize, MarketPolicy, ValuationMode, ValuationRequest, ValuationStatus, anomalies, candles,
    price_points, recommend_liquidity_anchors, summarize, value_against_anchor,
};
use ptt_trade_domain::{MarketAssetId, MarketEdgeObservation};
use ptt_trade_engine::{
    AssetAmount, AssetUnit, AssetUnitCatalog, ConversionRequest, FeePolicy, MarketDepthIndex,
    SearchCancellation, find_best_conversion,
};
use ptt_workflows::{
    FocusGroupItem, FocusRole, FocusScope, FocusScopePolicy, RadarBudget, RadarRequest, RadarStart,
    derive_focus_probe_candidates, run_opportunity_radar,
};

/// The sizes the convert page prices, in whole orbs.
const CONVERT_SIZES: [u64; 3] = [1, 10, 100];

/// Every path one search ranked: its pick first, then the rest.
fn ranked_paths(
    found: &Option<ptt_trade_engine::ConversionResult>,
) -> impl Iterator<Item = &ptt_trade_engine::ConversionPath> {
    found
        .iter()
        .flat_map(|result| result.best_path.iter().chain(result.alternatives.iter()))
}

/// The ask the route *search* runs at, whatever the reader happens to hold.
///
/// Only ever used to enumerate which paths exist. The engine drops a path
/// the moment a hop rounds to zero quanta, so searching at the reader's own
/// holding lets a dear bridge currency delete every route through itself --
/// see the comment in `convert_model`. Big enough that no bridge on a real
/// book rounds away, small enough to stay far from the overflow guards in
/// `AssetAmount::from_whole_units`.
const ROUTE_ENUMERATION_SIZE: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// Page models
//
// Every page is computed once into one of these and rendered twice: as the
// text lines below, and as the real interface in `ptt-app`. The split exists
// because the lines were the only form the answers had, so anything the
// renderer wanted that a line did not already say -- a per-leg breakdown, the
// seconds behind a freshness light, the rows a queue excluded -- had to be
// recomputed or parsed back out of prose.
//
// The models therefore hold the typed values, and the flattening functions
// hold every formatting decision. A page that needs a new number adds it here;
// it never re-derives one from a rendered string.
// ---------------------------------------------------------------------------

/// Prose the pages print before anything else: a configuration that could not
/// be used degrades loudly, never silently. Rendered at build time because
/// these are sentences, not values.
type Notes = Vec<String>;

/// "I hold X and want Y", answered at one or more sizes.
#[derive(Clone, Debug)]
pub struct ConvertModel {
    pub notes: Notes,
    pub have: MarketAssetId,
    pub need: MarketAssetId,
    /// One entry per size that could be priced at all. A size the unit
    /// catalogue cannot express is absent rather than present-and-empty.
    pub sizes: Vec<SizeRoute>,
    /// The listing advice for the middle size, when there was a book to give
    /// it. Advisory: its absence never invalidates the routes above.
    pub maker: Option<MakerModel>,
    /// Market-pulse context for the asset being acquired — the greedy card's
    /// evidence line (scarce? drifting up?). Absent without season history.
    pub need_structural: Option<StructuralNote>,
}

#[derive(Clone, Debug)]
pub struct SizeRoute {
    pub size: u64,
    /// Every route worth putting in front of the reader at this size, in the
    /// order `compare_paths` ranked them. The direct trade is always here;
    /// see [`route_quotes`] for what the others had to clear to join it.
    pub quotes: Vec<RouteQuote>,
    /// Every other candidate priced worse than going direct, so the list is
    /// down to the baseline alone. A conclusion — "nothing beats direct" —
    /// and not the same thing as finding no route, which is why it is a flag
    /// rather than an empty list.
    pub direct_is_the_only_one: bool,
}

impl SizeRoute {
    /// The one step every pinching route on this card pinches at, and how
    /// many routes that is — `None` when they pinch in different places, or
    /// when only one of them pinches at all.
    ///
    /// A book is hub-and-spoke, so the detours out of a currency nearly all
    /// leave through the same bridge and therefore pinch on the same step
    /// with the same two numbers. Printing one warning row per route was
    /// still a wall of thirteen identical sentences on the owner's real card.
    /// Saying it once and naming the count is strictly more information in
    /// one line than in thirteen.
    ///
    /// Routes that pinch nowhere are not counted and do not veto the
    /// collapse: they print no row either way, so they cannot be part of a
    /// wall. That is why the sentence carries a count rather than the words
    /// "every route" — a clean direct trade sitting beside three pinched
    /// detours must not be swept into a claim about all four.
    ///
    /// Lives on the model rather than in a renderer because this page has two
    /// of them, and a rule that lives in one is a rule the other drifts away
    /// from.
    #[must_use]
    pub fn shared_pinch(&self) -> Option<(&LegTakeCoverage, usize)> {
        let mut pinches = self.quotes.iter().filter_map(RouteQuote::pinch);
        let first = pinches.next()?;
        let mut count = 1_usize;
        for leg in pinches {
            // Whole-struct equality: two routes pinching on the same pair for
            // different amounts are two different warnings and must stay
            // apart, or the collapsed row would print numbers true of only
            // one of them.
            if leg != first {
                return None;
            }
            count += 1;
        }
        (count > 1).then_some((first, count))
    }
}

/// An exact rate: whole units of the target per whole unit of the source.
///
/// A fraction rather than a decimal because every comparison on this page is
/// between two of these, and 8.3 keeps floats at the drawing edge. Rendering
/// truncates ([`RouteRate::text`]); nothing that decides anything does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteRate {
    pub numerator: u128,
    pub denominator: u128,
}

/// One candidate route, priced at the rate the reader can actually list it
/// at.
///
/// **Rate first, quantity second, and never the other way round.** The
/// reader types one exchange rate into the game and lists it; the exchange
/// fills that listing at their rate or at a better one, or leaves it sitting.
/// An order does not slide down to the next level the way a sweep does, so
/// the number that decides whether a route is worth taking is its rate, and
/// how much of the reader's stack the market can absorb at that rate is a
/// separate warning printed beside it. The blended average of eating down
/// several levels is a different question — "what if I clear the shelf right
/// now" — and lives under its own label in the tiers below.
///
/// The direct consequence, and the invariant the tests pin: **the sign of
/// the profit never depends on the size of the ask.** Ten and five thousand
/// give the same percentage on the same book, because the percentage is a
/// ratio of two rates and neither of them knows the size.
#[derive(Clone, Debug)]
pub struct RouteQuote {
    pub route_asset_ids: Vec<MarketAssetId>,
    /// One hop: the trade the other routes are measured against.
    pub is_direct: bool,
    /// The front row of every leg, multiplied together. `None` when a leg had
    /// no priced front row, in which case nothing about this route is
    /// claimed — and it is still shown, because an unmeasured route is not a
    /// bad one.
    pub rate: Option<RouteRate>,
    /// `size` run through the legs at [`RouteQuote::rate`], flooring at each
    /// leg the way the game does.
    pub projected_output: Option<u64>,
    /// Signed basis points against the direct route's own front rate. Size
    /// plays no part: this is one rate over another.
    pub versus_direct_bps: Option<i64>,
    pub direction: Option<ptt_trade_engine::ComparisonDirection>,
    /// Difference from what the direct trade produces at this same size. The
    /// one number here that does scale with the ask, which is why the
    /// percentage beside it is computed separately rather than derived from
    /// this.
    pub delta_output: Option<u64>,
    /// Whole units of the starting asset the front rows absorb before the
    /// price moves off [`RouteQuote::rate`]. A warning, never a filter.
    pub fillable_input: Option<u64>,
    /// This route's legs against the listings each would have to take.
    pub legs: Vec<LegTakeCoverage>,
}

/// How far the listings on one leg go towards filling that leg *right now*.
///
/// Strictly a taker reading, and the naming is deliberate. Nothing here says
/// whether an order the reader lists will find a buyer: in this exchange a
/// listing is filled at the rate its owner named, or at a better one, or not
/// at all — so a maker never walks down a book and the question "will my
/// order sit there" is not a depth question. That risk is read off supply and
/// demand instead (`StructuralNote`), and is not what this measures.
///
/// The variants are in escalation order and the ordering is used: a middle
/// currency is judged by the tighter of the two books it passes through,
/// which is `max` over this enum. `NoListings` sits first on purpose —
/// "nobody ever captured this direction" must never be reported as "the
/// market is short", and last place would let it escalate a neighbour.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LegTakeVerdict {
    /// No listings on record in this direction. Not a shortage: an absence.
    NoListings,
    /// The listings hold it. Whatever share of the book that is.
    ///
    /// There used to be a band between this and the next one — "takes a
    /// large share, so the fill walks deep and the average worsens". That is
    /// a taker's hazard, and this page prices what the reader can *list* at:
    /// POE fills a listing at that rate or better, so the slide it warned
    /// about cannot reach them. It was ten of the fourteen warning rows on
    /// the owner's card, all about a thing that does not happen.
    Covered,
    /// More than everything listed — one pass cannot fill it at any price.
    NotEnoughListed,
}

/// One leg of a route, against the listings that leg would have to take.
///
/// The numbers are the point, not the verdict. A currency's total listings
/// say nothing about whether *this* leg can carry the trade: on 2026-08-23
/// the market held 3,133 left-erasure omens and the chaos leg to them held
/// 11, because the other 3,122 were listed against divine and that leg cannot
/// reach them. So the denominator here is the leg, never the currency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegTakeCoverage {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    /// Whole units of `to_asset_id` this trip has to take on this leg — the
    /// whole request, not the part of it that fit. A leg that ran out of
    /// listings halfway is the leg this signal exists to name, and scoring it
    /// by what it managed to fill would score it as if it had wanted less.
    pub taking: u64,
    /// Whole units of `to_asset_id` listed on the side this leg takes from.
    /// `None` when the direction was never captured.
    pub listed: Option<u64>,
    /// `taking` as a percent of `listed`, floored.
    pub share_percent: Option<u64>,
    pub verdict: LegTakeVerdict,
    /// The verdict came from the next leg rather than this one: the currency
    /// this leg buys is immediately spent taking the next one, and that book
    /// is the tighter of the two.
    pub bound_by_next_leg: bool,
    /// One listing backs the whole figure, so nothing corroborates it.
    pub single_listing: bool,
}

impl RouteQuote {
    /// The one step worth printing under this route, if any.
    ///
    /// The rate row already says how much the market absorbs at this rate,
    /// so the only thing the steps can add is *which* one is the narrow one.
    /// Preference goes to a step that earned its own verdict over one merely
    /// wearing its neighbour's, so the sentence on the row is always true of
    /// the two numbers printed beside it; among those, the one taking the
    /// largest share of what is listed against it.
    #[must_use]
    pub fn pinch(&self) -> Option<&LegTakeCoverage> {
        pinch_of(&self.legs)
    }
}

/// The selection rule behind [`RouteQuote::pinch`] and [`RouteWalk::pinch`],
/// written once so the Convert page and the radar's detail panel can never
/// disagree about which step a route pinches at.
fn pinch_of(legs: &[LegTakeCoverage]) -> Option<&LegTakeCoverage> {
    legs.iter()
        .filter(|leg| leg.is_noteworthy())
        .max_by(|left, right| {
            left.bound_by_next_leg
                .cmp(&right.bound_by_next_leg)
                .reverse()
                .then_with(|| compare_shortfall(left, right))
        })
}

/// Which of two steps is the tighter, by the share of its book it takes.
///
/// Cross-multiplied rather than divided, for the reason the engine gives
/// about every other ratio here: integer division rounds two different
/// shares into a tie the market never had. A step with nothing listed is
/// tighter than any step with something.
fn compare_shortfall(left: &LegTakeCoverage, right: &LegTakeCoverage) -> std::cmp::Ordering {
    match (left.listed, right.listed) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left_listed), Some(right_listed)) => (u128::from(left.taking)
            * u128::from(right_listed))
        .cmp(&(u128::from(right.taking) * u128::from(left_listed))),
    }
}

impl LegTakeCoverage {
    /// Whether this step has anything the route line above it does not say.
    ///
    /// The route already prints how much the market absorbs at its rate,
    /// which is the tightest of its steps folded back to the reader's asset.
    /// A step that clears, is not standing in for a tighter neighbour and
    /// has more than one listing behind it adds nothing to that -- and
    /// printing it anyway buries the steps that do.
    #[must_use]
    pub const fn is_noteworthy(&self) -> bool {
        !matches!(self.verdict, LegTakeVerdict::Covered)
            || self.bound_by_next_leg
            || self.single_listing
    }
}

/// One leg's front row and listed book, carried out of the scan.
///
/// The radar deliberately knows nothing about the reader's holdings — its
/// ruling is that it finds rates and the reader brings the size. This is the
/// bridge that makes the split workable: the model saves each leg's rate and
/// book while the market is still in hand, so the detail panel can price any
/// ask the reader types later without a second trip to the store.
#[derive(Clone, Debug)]
pub struct RouteLegBook {
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    /// The front row's rate. `None` when the direction has no priced row,
    /// which breaks the walk from this leg on rather than inventing one.
    pub rate: Option<ptt_trade_domain::Ratio>,
    /// Whole units of `from_asset_id` the front row alone can absorb.
    pub front_capacity: Option<u64>,
    /// Whole units of `to_asset_id` listed on the side this leg takes from.
    /// `None` when the direction was never captured.
    pub listed: Option<u64>,
    /// One listing backs the whole figure, so nothing corroborates it.
    pub single_listing: bool,
}

/// A typed ask walked through a route's saved front rows: what it projects
/// to, how much the books absorb at that rate, and where it pinches.
#[derive(Clone, Debug)]
pub struct RouteWalk {
    /// The composed front rate of the whole route.
    pub rate: Option<RouteRate>,
    /// The ask at that rate, floored once — see [`project_at_front_rates`]
    /// for why the floor never happens per hop.
    pub projected_output: Option<u64>,
    /// Whole units of the start asset the front rows absorb before the price
    /// moves off [`RouteWalk::rate`]. A warning, never a filter.
    pub fillable_input: Option<u64>,
    pub legs: Vec<LegTakeCoverage>,
}

impl RouteWalk {
    /// The one step worth printing, by the same rule as
    /// [`RouteQuote::pinch`].
    #[must_use]
    pub fn pinch(&self) -> Option<&LegTakeCoverage> {
        pinch_of(&self.legs)
    }
}

/// Walks `amount` through a route's saved front rows.
///
/// Pure arithmetic on [`RouteLegBook`]s, so the detail panel can re-run it on
/// every keystroke without touching the store. Same shape as the Convert
/// page's pricing on purpose: every step is measured against the ask through
/// the prefix of front rates, never against what the step before it managed
/// to fill, and the rate itself never depends on the ask.
#[must_use]
pub fn walk_route(legs: &[RouteLegBook], amount: u64) -> RouteWalk {
    if legs.is_empty() {
        return RouteWalk {
            rate: None,
            projected_output: None,
            fillable_input: None,
            legs: Vec::new(),
        };
    }
    let mut rate: Option<RouteRate> = Some(RouteRate::ONE);
    let mut fillable: Option<u64> = None;
    // One unknown capacity makes the whole figure unknown: "the thinnest of
    // the rows I could see" is not the thinnest row.
    let mut fillable_known = true;
    let mut takes: Vec<Option<u64>> = Vec::with_capacity(legs.len());
    for leg in legs {
        // This leg's front-row capacity, walked back through the legs before
        // it so it is denominated in the reader's own asset.
        let capacity = rate
            .zip(leg.front_capacity)
            .and_then(|(prefix, capacity)| prefix.back_to_source(capacity));
        match capacity {
            Some(capacity) => {
                fillable = Some(fillable.map_or(capacity, |held| held.min(capacity)));
            }
            None => fillable_known = false,
        }
        rate = rate
            .zip(leg.rate.as_ref())
            .and_then(|(prefix, leg_rate)| prefix.times(leg_rate));
        takes.push(rate.and_then(|composed| composed.forward(amount)));
    }
    let mut coverage: Vec<LegTakeCoverage> = legs
        .iter()
        .zip(&takes)
        .map(|(leg, take)| {
            let taking = take.unwrap_or(0);
            let listed = leg.listed.unwrap_or(0);
            LegTakeCoverage {
                from_asset_id: leg.from_asset_id.clone(),
                to_asset_id: leg.to_asset_id.clone(),
                taking,
                listed: leg.listed,
                // Floored, and left off under one percent, for the reason
                // `route_leg_coverage` gives: "0%" beside a five-figure
                // amount reads as a broken number rather than a small one.
                share_percent: (listed > 0)
                    .then(|| {
                        u64::try_from(u128::from(taking) * 100 / u128::from(listed))
                            .unwrap_or(u64::MAX)
                    })
                    .filter(|share| *share > 0),
                verdict: leg_take_verdict(taking, listed),
                bound_by_next_leg: false,
                single_listing: leg.single_listing,
            }
        })
        .collect();
    escalate_middle_legs(&mut coverage);
    RouteWalk {
        rate,
        projected_output: rate.and_then(|rate| rate.forward(amount)),
        fillable_input: if fillable_known { fillable } else { None },
        legs: coverage,
    }
}

/// The trader's three ways to act on a pair as a maker, priced against taking
/// the instant fill now.
#[derive(Clone, Debug)]
pub struct MakerModel {
    pub size: u64,
    pub strategy: ptt_strategy::MakerStrategy,
    /// The same Opportunity mode priced at the front instead of below it — a
    /// second evaluation, so the trade-off stays visible without a third mode
    /// existing anywhere.
    pub match_front: Option<ptt_strategy::MakerRecommendation>,
    /// The risks every drawn mode carries, so the panel says them once.
    ///
    /// A hazard that holds for the undercut, the match and the greedy listing
    /// alike is a property of the pair — the quote is an aggregate row, the
    /// book is one listing deep — not of where inside the queue you price.
    /// Printed per mode it was the same sentence three times, which buried
    /// the one thing a mode row can say for itself.
    ///
    /// The **intersection of the drawn rows**, not `strategy.assessment`:
    /// that one adds hazards about excluded listings no mode row ever
    /// carried. And an intersection rather than "they are always the same",
    /// because they are not — only the greedy listing can sit behind a wall,
    /// and only the undercut can fail to undercut. Such a remainder stays on
    /// its own row rather than being promoted to a claim about the pair.
    ///
    /// Read by the text report only. The GPUI panel draws no risk text at all
    /// and deliberately still does not: this page was just cut down to focus
    /// on rates, and the way to honour that is not to add a warning row to it
    /// — the same reason the all-clear leg chips went. If it ever grows one,
    /// it must use this field and [`MakerModel::mode_only_risks`] rather than
    /// `blocking()`, or the two renderers will disagree about whose risk it is.
    pub shared_risks: Vec<ptt_strategy::ExecutionRisk>,
}

impl MakerModel {
    /// What one mode adds on top of [`MakerModel::shared_risks`] — usually
    /// nothing, which is the point.
    #[must_use]
    pub fn mode_only_risks(
        &self,
        recommendation: &ptt_strategy::MakerRecommendation,
    ) -> Vec<ptt_strategy::ExecutionRisk> {
        recommendation
            .assessment
            .blocking()
            .into_iter()
            .filter(|risk| !self.shared_risks.contains(risk))
            .collect()
    }
}

/// "Is what I am watching healthy": coverage, valuations, and what to promote.
#[derive(Clone, Debug)]
pub struct WatchlistModel {
    pub notes: Notes,
    pub core_liquidity: Vec<MarketAssetId>,
    pub valuations: Vec<AssetValuation>,
    pub coverage: CoverageOutcome,
    pub suggestions: Vec<FocusSuggestion>,
    pub anchors: Vec<ptt_strategy::AnchorRecommendation>,
}

#[derive(Clone, Debug)]
pub struct AssetValuation {
    pub asset_id: MarketAssetId,
    pub valuation: ptt_strategy::Valuation,
}

/// A currency with real buy pressure that is not in the focus list.
///
/// The evidence is the listed quantities on the two book sides, valued in
/// the primary settlement currency — capture frequency proves only that the
/// user looked, not that anyone wants the thing (user-adjudicated 2026-08-22).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusSuggestion {
    pub asset_id: MarketAssetId,
    /// Anchor-valued demand-side listings in the window: the buy pressure.
    pub demand_anchor: u64,
    /// Anchor-valued supply-side listings, for the same window.
    pub supply_anchor: u64,
}

/// Coverage is a third view of the book and can fail on its own; the page
/// still has valuations to show when it does.
#[derive(Clone, Debug)]
pub enum CoverageOutcome {
    /// No anchor currency, so there was nothing to measure coverage against.
    NotComputed,
    Failed(String),
    Ready(CoverageModel),
}

#[derive(Clone, Debug)]
pub struct CoverageModel {
    /// Whether the scope was answerable at all.
    ///
    /// Carried rather than inferred from an entry count: a list that names
    /// only currencies the settlement set already covers produces a scope with
    /// no targets, and the two pairs left over — the settlement currencies
    /// against each other — look exactly like a market nobody has captured.
    pub status: ptt_workflows::FocusScopeStatus,
    pub entries: Vec<ptt_workflows::FocusCoverage>,
    pub candidates: Vec<ptt_workflows::ProbeCandidate>,
}

/// "Where is the money right now": the unified radar.
#[derive(Clone, Debug)]
pub struct OpportunitiesModel {
    pub notes: Notes,
    pub scan: RadarScan,
}

#[derive(Clone, Debug)]
pub enum RadarScan {
    /// The radar could not run, and why. Distinct from running and finding
    /// nothing: one is a gap in the data, the other is an answer.
    Unavailable(RadarUnavailable),
    Ran(Box<RadarScanResult>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RadarUnavailable {
    /// No settlement currency, so there is nowhere to start or return to.
    NoCoreCurrency,
    NotEnoughMarket,
    /// Every settlement currency is missing a unit — the window has data,
    /// just none of it touching a currency the scan could start from.
    NoStartUnits { anchor: Option<MarketAssetId> },
}

#[derive(Clone, Debug)]
pub struct RadarScanResult {
    pub starts: Vec<MarketAssetId>,
    pub items: Vec<OpportunityRow>,
    pub probe_candidates: Vec<ptt_workflows::ProbeCandidate>,
    pub diagnostics: ptt_workflows::RadarDiagnostics,
}

#[derive(Clone, Debug)]
pub struct OpportunityRow {
    pub item: ptt_workflows::RadarItem,
    /// Read from the oldest leg: a route is only as current as the capture it
    /// leans on hardest.
    pub light: Option<FreshnessStatus>,
    /// Season-scale liquidity context for each leg asset the market pulse
    /// knows. Advisory only: it never reorders items or changes a category —
    /// the current snapshot depth is real even when a leg's market is
    /// structurally thin (user ruling: sort stays liquidity > profit > hops).
    pub structural: Vec<StructuralNote>,
    /// Each leg's front row and book, saved so the detail panel can price
    /// whatever ask the reader types there ([`walk_route`]) — the radar
    /// itself never assumes a size on their behalf.
    pub leg_books: Vec<RouteLegBook>,
}

/// One asset's market-pulse context, attached to a route or a pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralNote {
    pub asset_id: MarketAssetId,
    pub class: ptt_strategy::LiquidityClass,
    pub verdict: Option<ptt_strategy::TrendVerdict>,
    /// Scarce and not depreciating relative to the market: the greedy-mode
    /// precondition holds for this asset.
    pub greedy_candidate: bool,
}

impl StructuralNote {
    /// A structurally thin leg: nothing wrong with the printed numbers, but
    /// the asset's market is oversupplied or quiet season-wide, so refills
    /// behind the visible book are less likely.
    #[must_use]
    pub const fn structurally_illiquid(&self) -> bool {
        matches!(
            self.class,
            ptt_strategy::LiquidityClass::Oversupplied | ptt_strategy::LiquidityClass::Quiet
        )
    }
}

/// The pulse context for every path asset the pulse knows, in path order,
/// deduplicated. Annotation only — the caller must not resort on it.
fn structural_notes_for(
    path: &[MarketAssetId],
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Vec<StructuralNote> {
    let Some(pulse) = pulse else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut notes = Vec::new();
    for asset_id in path {
        if !seen.insert(asset_id.clone()) {
            continue;
        }
        if let Some(asset) = pulse
            .assets
            .iter()
            .find(|asset| asset.asset_id == *asset_id)
        {
            notes.push(StructuralNote {
                asset_id: asset.asset_id.clone(),
                class: asset.class,
                verdict: asset.verdict,
                greedy_candidate: asset.greedy_candidate,
            });
        }
    }
    notes
}

/// "What should I go look at next": the probe queue on its own.
#[derive(Clone, Debug)]
pub struct ProbeQueueModel {
    /// Nothing seen in the window at all, which is a different message from
    /// having nothing left to probe.
    pub nothing_captured: bool,
    pub complete_pairs: usize,
    pub total_pairs: usize,
    pub candidates: Vec<ptt_workflows::ProbeCandidate>,
}

/// "What has this pair been doing": candles, a summary, and what looks off.
#[derive(Clone, Debug)]
pub struct HistoryModel {
    pub notes: Notes,
    pub have: MarketAssetId,
    pub need: MarketAssetId,
    /// `None` when this direction has no priced points yet.
    pub summary: Option<ptt_strategy::PriceSummary>,
    /// Reads the newest capture of this direction: older points are history,
    /// but the light describes what acting now would lean on.
    pub light: Option<FreshnessStatus>,
    /// Every candle, oldest first. The text below shows the last few; a chart
    /// wants all of them.
    pub candles: Vec<ptt_strategy::PriceCandle>,
    pub anomalies: Vec<ptt_strategy::PriceAnomaly>,
}

/// "What is the market doing": season-scale value, supply/demand pressure,
/// and anchor health, built from persisted day rollups plus a live fold of
/// today. The only page whose data reaches past the report window.
#[derive(Clone, Debug)]
pub struct AnalyticsModel {
    pub notes: Notes,
    /// The active season banner, when one is configured.
    pub season: Option<SeasonBanner>,
    /// Distinct UTC days feeding the pulse (rollups + today).
    pub data_days: u32,
    pub pulse: ptt_strategy::MarketPulse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeasonBanner {
    pub label: String,
    pub started_day: String,
}

/// Everything the engine needs, assembled once from stored observations.
struct Market {
    index: MarketDepthIndex,
    /// Lines the pages print before anything else — a tuned policy that
    /// could not be used degrades loudly, never silently.
    notes: Vec<String>,
    units: AssetUnitCatalog,
    selected: Vec<EvaluatedQuoteEdge>,
    /// Kept so coverage can reuse them. Building the book clones every
    /// observation in the window, so doing it twice for one page doubled the
    /// most expensive step on the UI thread.
    book: ptt_market_book::CoherentCurrentBook,
    instant_selection: ptt_market_book::QuoteSelectionResult,
}

/// The tuned selection policy for one strategy, or the shipped default and
/// a visible note when the tuning does not validate.
fn selection_policy_from(
    tuning: &MarketTuning,
    strategy: QuoteSelectionStrategy,
    language: UiLanguage,
) -> Result<(QuoteSelectionPolicy, Option<String>), String> {
    let text = crate::report_text::report(language);
    let tuned = FreshnessPolicy::try_new(
        tuning.freshness.fresh_seconds,
        tuning.freshness.usable_seconds,
        tuning.freshness.stale_seconds,
    )
    .ok()
    .and_then(|freshness| {
        QuoteSelectionPolicy::personal_tuned(
            strategy,
            freshness,
            tuning.freshness.capture_skew_seconds,
            tuning.risk.top_book_outlier_factor,
        )
        .ok()
    });
    if let Some(policy) = tuned {
        return Ok((policy, None));
    }
    let policy = QuoteSelectionPolicy::personal_default(strategy)
        .map_err(|error| format!("policy: {error}"))?;
    Ok((policy, Some(text.freshness_config_invalid.to_owned())))
}

fn build_market(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Market, String> {
    let book = build_coherent_current_book(context_key, observations, DataVisibility::default())
        .map_err(|error| format!("book: {error}"))?;
    let (policy, policy_note) =
        selection_policy_from(tuning, QuoteSelectionStrategy::Instant, language)?;
    let selection = select_quote_edges(&book, &policy, Utc::now())
        .map_err(|error| format!("select: {error}"))?;

    let mut units = BTreeMap::new();
    for observation in observations {
        units.insert(observation.edge.from_asset_id.clone(), AssetUnit::whole());
        units.insert(observation.edge.to_asset_id.clone(), AssetUnit::whole());
    }
    let units = AssetUnitCatalog::try_new(units).map_err(|error| format!("units: {error}"))?;
    let index = MarketDepthIndex::try_from_selection(&selection, units.clone())
        .map_err(|error| format!("index: {error}"))?;

    // The selected edge is one of the candidates, so the candidates alone
    // carry it — taking it separately double-counted every selected edge in
    // valuations and price histories.
    let selected: Vec<_> = selection
        .selections
        .iter()
        .flat_map(|entry| entry.candidate_edges.iter().cloned())
        .collect();

    Ok(Market {
        index,
        notes: policy_note.into_iter().collect(),
        units,
        selected,
        book,
        instant_selection: selection,
    })
}

/// Whole units of `to` the market is showing on the side this leg buys from,
/// and how many rows say so.
///
/// **Not** `MarketDepthIndex::listed_liquidity`, which looks like exactly this
/// method and is not. That one sums every candidate row in the direction, and
/// the two sides of a panel count their stock in different currencies — the
/// available rows in what their lister pays out (the asset the leg wants), the
/// competing rows in what theirs pays out (the asset the leg spends). Adding
/// them lands in no unit at all, and because both edges of a row carry the
/// same stock the sum comes out identical in both directions, so it cannot
/// tell "buying B" from "selling B" either. On the 2026-08-23 book it answered
/// 3,133 for the chaos leg to left-erasure omens; the omens that leg can
/// actually reach numbered 11.
///
/// Every taker row counts, including the ones the depth walk refuses — the
/// aggregate `>` row is where most of a currency usually sits, and it is still
/// currency on the market. Same reasoning as `PairDepth::listed_stock`, minus
/// the side that is denominated in the wrong asset.
fn leg_book(
    selection: &ptt_market_book::QuoteSelectionResult,
    from: &MarketAssetId,
    to: &MarketAssetId,
) -> (u64, usize) {
    let Some(entry) = selection
        .selections
        .iter()
        .find(|entry| &entry.from_asset_id == from && &entry.to_asset_id == to)
    else {
        return (0, 0);
    };
    entry
        .candidate_edges
        .iter()
        .filter(|candidate| {
            candidate.observation.edge.execution_type == ptt_trade_domain::ExecutionType::Taker
                && candidate.observation.edge.context_key == selection.context_key
        })
        .fold((0_u64, 0_usize), |(stock, rows), candidate| {
            (
                stock.saturating_add(candidate.observation.edge.stock),
                rows + 1,
            )
        })
}

/// Whole units of `to` this leg has to take, for the whole request rather
/// than the part of it that fit.
///
/// **Every step is priced off the reader's ask and the front rows, never off
/// what the step before it managed to buy.** The engine hands each leg the
/// previous leg's *actual* output, so one short leg shrinks the request of
/// every leg behind it and they all report a smaller trip than was asked
/// for -- on the owner's real book the last leg of a three-hop route read
/// the same 27 at a 500 ask and at a 50,000 ask, which is a warning that
/// cannot warn. It also put the two halves of one card on different trips:
/// the header says "500 -> 88" from `project_at_front_rates` while the steps
/// described a walk that stopped at 27.
///
/// So the prefix product of the front rates carries the ask forward, one leg
/// at a time, and each entry is what that leg would have to buy. Floored once
/// per prefix rather than once per hop, for the reason
/// [`project_at_front_rates`] gives: flooring every hop can make a better
/// rate project less.
fn front_rate_takes(
    path: &ptt_trade_engine::ConversionPath,
    index: &MarketDepthIndex,
    size: u64,
) -> Vec<Option<u64>> {
    let mut rate = RouteRate::ONE;
    let mut takes = Vec::with_capacity(path.steps.len());
    for step in &path.steps {
        let next = leg_front_row(step, index).and_then(|(leg_rate, _)| rate.times(&leg_rate));
        match next {
            Some(composed) => {
                rate = composed;
                takes.push(rate.forward(size));
            }
            // A leg with no front row prices nothing behind it either: the
            // product is broken from here on, and guessing past it would
            // invent a number the book never said.
            None => takes.push(None),
        }
    }
    takes
}

/// The band one leg falls in: whether the listings hold this trip at all.
///
/// Two bands and no middle one. The only thing a lister cannot get around is
/// nobody being on the other side — what fraction of the book they take is
/// their business, and buying ten of the forty-one mirrors in existence is
/// 24% of the market and is also just a trade. The share is still printed;
/// there is simply no warning attached to it.
///
/// Compared on the exact integers, not the floored percentage: "more than
/// the market is showing" is a fact about two numbers, and 1,005 out of
/// 1,000 rounds to 100%.
const fn leg_take_verdict(taking: u64, listed: u64) -> LegTakeVerdict {
    if listed == 0 {
        LegTakeVerdict::NoListings
    } else if taking > listed {
        LegTakeVerdict::NotEnoughListed
    } else {
        LegTakeVerdict::Covered
    }
}

/// Every leg of one route, measured against the listings it would take.
///
/// The second pass is the middle-currency rule. A currency picked up in the
/// middle of a route is immediately spent taking the next one, and that
/// second book can be the tighter of the two — the route is only as walkable
/// in one pass as its narrowest book. So a leg that buys a middle currency
/// inherits its neighbour's verdict when the neighbour is worse. It keeps
/// printing its own numbers: the reader is owed both sides, and a leg
/// silently wearing a colour its own figures do not explain is the confusing
/// half of this.
fn route_leg_coverage(
    market: &Market,
    path: &ptt_trade_engine::ConversionPath,
    size: u64,
) -> Vec<LegTakeCoverage> {
    let takes = front_rate_takes(path, &market.index, size);
    let mut legs: Vec<LegTakeCoverage> = path
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let (from, to) = (&step.from_asset_id, &step.to_asset_id);
            let (listed, rows) = leg_book(&market.instant_selection, from, to);

            // The walked fill is the fallback, not the source: it is only
            // reached when a leg has no front row to price the ask through.
            let taking = takes.get(index).copied().flatten().unwrap_or(
                step.net_amount_out
                    .as_ref()
                    .unwrap_or(&step.gross_amount_out)
                    .quanta,
            );
            LegTakeCoverage {
                from_asset_id: from.clone(),
                to_asset_id: to.clone(),
                taking,
                listed: (listed > 0).then_some(listed),
                // Floored, so anything under one percent reads "0%" beside a
                // five-figure amount and looks like a broken number rather
                // than a small one. Under one percent the share says nothing
                // the two amounts do not already say, so it is left off for
                // the same reason it is left off above the whole book.
                share_percent: (listed > 0)
                    .then(|| {
                        u64::try_from(u128::from(taking) * 100 / u128::from(listed))
                            .unwrap_or(u64::MAX)
                    })
                    .filter(|share| *share > 0),
                verdict: leg_take_verdict(taking, listed),
                bound_by_next_leg: false,
                single_listing: rows == 1,
            }
        })
        .collect();

    escalate_middle_legs(&mut legs);
    legs
}

/// The middle-currency rule, shared with [`walk_route`]: a currency picked up
/// in the middle of a route is immediately spent taking the next leg, and
/// that second book can be the tighter of the two — so a leg that buys a
/// middle currency inherits its neighbour's verdict when the neighbour is
/// worse, while keeping its own numbers on display.
fn escalate_middle_legs(legs: &mut [LegTakeCoverage]) {
    let own: Vec<LegTakeVerdict> = legs.iter().map(|leg| leg.verdict).collect();
    for (index, leg) in legs.iter_mut().enumerate() {
        // "Never captured" is not evidence about anything, so it neither
        // escalates a neighbour nor gets escalated by one.
        let Some(exit) = own.get(index + 1).copied() else {
            continue;
        };
        if leg.verdict == LegTakeVerdict::NoListings || exit <= leg.verdict {
            continue;
        }
        leg.verdict = exit;
        leg.bound_by_next_leg = true;
    }
}

impl RouteRate {
    const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// Kept in lowest terms after every multiply, which is what keeps the
    /// four-hop products inside `u128` and the comparisons below total.
    fn reduced(numerator: u128, denominator: u128) -> Option<Self> {
        if denominator == 0 || numerator == 0 {
            return None;
        }
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            let next = a % b;
            a = b;
            b = next;
        }
        Some(Self {
            numerator: numerator / a,
            denominator: denominator / a,
        })
    }

    fn times(self, rate: &ptt_trade_domain::Ratio) -> Option<Self> {
        Self::reduced(
            self.numerator.checked_mul(u128::from(rate.numerator))?,
            self.denominator.checked_mul(u128::from(rate.denominator))?,
        )
    }

    /// `amount` of the target asset, expressed back in the source asset.
    fn back_to_source(self, amount: u64) -> Option<u64> {
        u64::try_from(u128::from(amount).checked_mul(self.denominator)? / self.numerator).ok()
    }

    /// `amount` of the source asset, forward into the target asset, floored.
    fn forward(self, amount: u64) -> Option<u64> {
        u64::try_from(u128::from(amount).checked_mul(self.numerator)? / self.denominator).ok()
    }

    fn compare(self, other: Self) -> std::cmp::Ordering {
        // Cross-multiplied, never divided: integer division would round two
        // rates the market never tied into a tie, and it is a tie that
        // decides whether a route is shown at all.
        (self.numerator * other.denominator).cmp(&(other.numerator * self.denominator))
    }

    /// This rate against `baseline`, in signed basis points.
    ///
    /// Two rates in, one ratio out — the ask never enters, which is the whole
    /// reason this is computed from rates rather than from the two projected
    /// outputs. Flooring the outputs first would make the same route read
    /// -0.93% at ten and -0.02% at five thousand.
    fn versus(self, baseline: Self) -> Option<i64> {
        let mine = i128::try_from(self.numerator.checked_mul(baseline.denominator)?).ok()?;
        let theirs = i128::try_from(baseline.numerator.checked_mul(self.denominator)?).ok()?;
        if theirs == 0 {
            return None;
        }
        i64::try_from(mine.checked_sub(theirs)?.checked_mul(10_000)? / theirs).ok()
    }

    /// Up to four decimals, truncated, trailing zeros trimmed.
    fn decimal(numerator: u128, denominator: u128) -> String {
        let whole = numerator / denominator;
        let remainder = numerator % denominator;
        let Some(scaled) = remainder.checked_mul(10_000) else {
            return whole.to_string();
        };
        let fraction = scaled / denominator;
        if fraction == 0 {
            return whole.to_string();
        }
        format!("{whole}.{fraction:04}")
            .trim_end_matches('0')
            .to_owned()
    }

    /// The rate the way the game writes one: `10.8 : 1`, or `1 : 57` when
    /// the target is the dearer of the two.
    ///
    /// Whichever side is bigger takes the decimals. Forcing every rate into
    /// `x : 1` prints an omen at `0.0175 : 1`, and four decimals of a number
    /// that small is a quarter of a percent of error on a figure the reader
    /// is about to compare against another one.
    #[must_use]
    pub fn text(self) -> String {
        if self.numerator >= self.denominator {
            format!("{} : 1", Self::decimal(self.numerator, self.denominator))
        } else {
            format!("1 : {}", Self::decimal(self.denominator, self.numerator))
        }
    }
}

/// The front row of one leg: the rate it quotes, and how much of the leg's
/// input that row alone can absorb.
///
/// `fills` is the depth walk's own record and its first entry is the best
/// level it used, so this is the same row the engine priced against. The
/// fallback is for a leg that consumed nothing at all — no fills to read,
/// but the book still has a front row and the reader can still list at it.
fn leg_front_row(
    step: &ptt_trade_engine::PairFill,
    index: &MarketDepthIndex,
) -> Option<(ptt_trade_domain::Ratio, u64)> {
    if let Some(front) = step.fills.first() {
        return Some((front.rate.clone(), front.capacity_from.quanta));
    }
    let (rate, stock) = index.top_of_book(&step.from_asset_id, &step.to_asset_id)?;
    // A taker row's stock counts the asset the leg buys (CORE-TRADING-MODEL
    // 7.1), so walk it back to what the leg has to spend to take it.
    let capacity = u64::try_from(
        u128::from(stock).checked_mul(u128::from(rate.denominator))? / u128::from(rate.numerator),
    )
    .ok()?;
    Some((rate, capacity))
}

/// Each leg of a scanned route, read off the market while it is still in
/// hand, in the form [`walk_route`] prices asks against later.
///
/// Reads through [`leg_front_row`] and [`leg_book`] so the bridge and the
/// Convert page are looking at the same rows: a route must not price one way
/// on the page that found it and another on the page that evaluates it.
fn route_leg_books(market: &Market, steps: &[ptt_trade_engine::PairFill]) -> Vec<RouteLegBook> {
    steps
        .iter()
        .map(|step| {
            let front = leg_front_row(step, &market.index);
            let (listed, rows) =
                leg_book(&market.instant_selection, &step.from_asset_id, &step.to_asset_id);
            RouteLegBook {
                from_asset_id: step.from_asset_id.clone(),
                to_asset_id: step.to_asset_id.clone(),
                rate: front.as_ref().map(|(rate, _)| rate.clone()),
                front_capacity: front.map(|(_, capacity)| capacity),
                listed: (listed > 0).then_some(listed),
                single_listing: rows == 1,
            }
        })
        .collect()
}

/// The rate a whole route can be listed at, and how much of the starting
/// asset the front rows absorb at it.
///
/// Every leg contributes its front row and nothing deeper. The reader lists
/// one rate per leg and waits; nobody fills them at a worse price than they
/// asked for, so the levels behind the front are not part of the price they
/// get — they are part of how long they will be waiting, which is the
/// capacity half of the answer.
fn route_front_quote(
    path: &ptt_trade_engine::ConversionPath,
    index: &MarketDepthIndex,
) -> Option<(RouteRate, u64)> {
    let mut rate = RouteRate::ONE;
    let mut fillable: Option<u64> = None;
    for step in &path.steps {
        let (leg_rate, leg_capacity) = leg_front_row(step, index)?;
        // This leg's capacity is denominated in what *it* spends, so it has
        // to travel back through the legs before it to be comparable with
        // the reader's stack.
        let in_source = rate.back_to_source(leg_capacity)?;
        fillable = Some(fillable.map_or(in_source, |least| least.min(in_source)));
        rate = rate.times(&leg_rate)?;
    }
    Some((rate, fillable?))
}

/// `size` at the route's own rate: one multiply, one floor.
///
/// Deliberately **not** flooring at every leg. Rounding each hop down models
/// a literal three-orb walk faithfully and then contradicts the line beside
/// it: on the 2026-08-23 book a three-hop route 3.06% ahead of direct on rate
/// projected *fewer* chaos than direct at a size of three, because two
/// intermediate floors ate more than the edge was worth. A row that reads
/// "3.06% better" and shows a smaller number is worse than a row that is
/// approximate.
///
/// One floor over the composed rate is monotone in that rate — a better rate
/// can never project less — so the projection, the delta and the percentage
/// always agree. It is also exactly what the ruling asks for: output = size ×
/// the rate you can list at.
fn project_at_front_rates(
    path: &ptt_trade_engine::ConversionPath,
    index: &MarketDepthIndex,
    size: u64,
) -> Option<u64> {
    route_front_quote(path, index).and_then(|(rate, _)| rate.forward(size))
}

/// Every route the reader should see at one size, in the order the engine
/// ranked them.
///
/// **Two rules, and the order between them is the whole design.**
///
/// *Rank does not decide visibility.* A route that came fourth is still a
/// rate the reader can list at, and hiding it because a sort put it fourth
/// is how a real opportunity gets thrown away — the program does not know
/// that an orb is the league's store of value, so when in doubt it shows.
///
/// *Losing on rate does decide visibility.* A route whose reachable rate is
/// worse than simply trading direct is not an opportunity at any size, so it
/// is dropped rather than shown as a negative number nobody wants.
///
/// **The second rule is only safe because of the first half of this file.**
/// While the profit figure was the blended average of a sweep, a route with
/// an excellent rate that could only fill a sliver of the ask computed as a
/// loss — hiding negatives then would have hidden exactly the opportunities
/// the first rule exists to protect. Now the sign comes from the rates alone
/// and the size lands entirely in the liquidity notes, so a route is dropped
/// only for being genuinely worse to trade at. **Anyone who makes the profit
/// figure depend on the ask again must delete this filter in the same
/// change.**
///
/// Liquidity never removes anything. Not enough listed is a note beside the
/// route, never a reason to withhold it: whether to list into a thin book is
/// the reader's call, and they can only make it if they can see the book.
fn route_quotes<'a>(
    market: &Market,
    candidates: &[&'a ptt_trade_engine::ConversionPath],
    direct: Option<&ptt_trade_engine::ConversionPath>,
    size: u64,
) -> Vec<(&'a ptt_trade_engine::ConversionPath, RouteQuote)> {
    let baseline = direct.and_then(|path| route_front_quote(path, &market.index));
    let baseline_rate = baseline.map(|(rate, _)| rate);
    let baseline_output = direct.and_then(|path| project_at_front_rates(path, &market.index, size));

    let mut quotes = Vec::new();
    for path in candidates {
        let is_direct = path.steps.len() == 1;
        let front = route_front_quote(path, &market.index);
        let rate = front.map(|(rate, _)| rate);
        if let (false, Some(rate), Some(baseline)) = (is_direct, rate, baseline_rate) {
            if rate.compare(baseline) == std::cmp::Ordering::Less {
                continue;
            }
        }
        let projected = project_at_front_rates(path, &market.index, size);
        let versus_direct_bps = match (rate, baseline_rate) {
            (Some(rate), Some(baseline)) => rate.versus(baseline),
            _ => None,
        };
        let direction = versus_direct_bps.map(|points| match points.cmp(&0) {
            std::cmp::Ordering::Greater => ptt_trade_engine::ComparisonDirection::Improved,
            std::cmp::Ordering::Equal => ptt_trade_engine::ComparisonDirection::Equal,
            std::cmp::Ordering::Less => ptt_trade_engine::ComparisonDirection::Worse,
        });
        quotes.push((
            *path,
            RouteQuote {
                route_asset_ids: path.path_asset_ids.clone(),
                is_direct,
                rate,
                projected_output: projected,
                versus_direct_bps,
                direction,
                delta_output: match (projected, baseline_output) {
                    (Some(mine), Some(theirs)) => Some(mine.abs_diff(theirs)),
                    _ => None,
                },
                fillable_input: front.map(|(_, fillable)| fillable),
                legs: route_leg_coverage(market, path, size),
            },
        ));
    }
    // Ordered by the number printed on the rows: best rate first, the direct
    // baseline at the bottom as the floor everything above it beat. The
    // engine's own order ranks what this particular size realized -- observed
    // stranding, then the blended price of the fill -- and both of those move
    // with the ask, so borrowing it made the rows shuffle when the holding
    // changed. Rate is the one key on this page the ask cannot touch. What
    // the engine's liquidity key knew is not lost: it is the leg chips beside
    // each row, where the reader weighs it against the rate themselves.
    quotes.sort_by(|(left_path, left), (right_path, right)| {
        match (left.rate, right.rate) {
            (Some(mine), Some(theirs)) => theirs.compare(mine),
            // A route without a front price has nothing to stand on the rate
            // ladder with; it sinks below everything that does.
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        // Equal rates: direct sinks to the floor regardless. It is the row
        // every other row is measured against, and a baseline sitting in the
        // middle of the list is the symptom this sort exists to remove --
        // ordering ties by step count put a one-step direct back on top of
        // every route that merely matched it. Among the rest the shorter
        // route wins, since a detour earning zero basis points has not paid
        // for its extra book, and asset ids settle what is left so the order
        // never depends on how the candidates arrived.
        .then_with(|| left.is_direct.cmp(&right.is_direct))
        .then_with(|| left_path.steps.len().cmp(&right_path.steps.len()))
        .then_with(|| left_path.path_asset_ids.cmp(&right_path.path_asset_ids))
    });
    quotes
}

/// One candidate route as a line: name, rate, standing against direct, what
/// this size projects to, and how deep the front rows are.
///
/// The direct trade carries the word "baseline" where the others carry a
/// percentage, because `+0.00%` against itself is a number that invites the
/// reader to compare it with something.
fn quote_line(quote: &RouteQuote, size: u64, have: &MarketAssetId, language: UiLanguage) -> String {
    let text = crate::report_text::report(language);
    let hops: Vec<String> = quote
        .route_asset_ids
        .iter()
        .skip(1)
        .take(quote.route_asset_ids.len().saturating_sub(2))
        .map(|asset| asset.as_str().to_owned())
        .collect();
    let label = crate::report_text::route_quote_label(language, &hops);
    let rate = quote
        .rate
        .map_or_else(|| text.route_no_front_price.to_owned(), RouteRate::text);
    let standing = if quote.is_direct {
        text.route_baseline.to_owned()
    } else {
        crate::report_text::versus_direct(
            language,
            quote.direction,
            quote.delta_output,
            quote.versus_direct_bps,
        )
    };
    let projected = quote
        .projected_output
        .map_or_else(|| "-".to_owned(), |out| format!("{size} -> {out}"));
    let depth = crate::report_text::join_text(
        language,
        &crate::report_text::route_depth_notes(language, quote, size, have.as_str())
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    format!("{label:<28} {rate:>14}   {standing:<36} {projected:<18} {depth}")
}

/// "I hold X and want Y": every rate the book can be routed at, and how much
/// of the reader's asset the market absorbs at each.
pub fn convert_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    holdings: Option<u64>,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = convert_model(
        observations,
        context_key,
        have,
        need,
        holdings,
        tuning,
        language,
        None,
    )?;
    Ok(render_convert(&model, language))
}

/// The sizes the page prices.
///
/// A stated holding prices exactly that size — "I have 100 divine" is a
/// question about 100, not about a ladder. Otherwise the configured ladder,
/// with the shipped one behind an empty or zeroed setting.
fn convert_sizes(holdings: Option<u64>, tuning: &MarketTuning) -> Vec<u64> {
    match holdings {
        Some(count) => vec![count.max(1)],
        None => {
            let configured: Vec<u64> = tuning
                .convert
                .sizes
                .iter()
                .copied()
                .filter(|size| *size > 0)
                .collect();
            if configured.is_empty() {
                CONVERT_SIZES.to_vec()
            } else {
                configured
            }
        }
    }
}

/// Everything the Convert page knows, before anything decides how to draw it.
#[allow(clippy::too_many_arguments)]
pub fn convert_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    holdings: Option<u64>,
    tuning: &MarketTuning,
    language: UiLanguage,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Result<ConvertModel, String> {
    // "X into X" is not a route question. The engine says so with
    // `InvalidAnalysisRequest`, which is correct of it — that is a caller
    // that should have known better — but propagating it turns a reader
    // setting both pickers to the same currency into a page-wide fault with
    // no way back out. The one useful sentence, said here, instead.
    if have == need {
        return Ok(ConvertModel {
            notes: vec![
                crate::report_text::report(language)
                    .same_currency
                    .to_owned(),
            ],
            have: have.clone(),
            need: need.clone(),
            sizes: Vec::new(),
            maker: None,
            need_structural: None,
        });
    }
    let market = build_market(observations, context_key, tuning, language)?;
    let sizes = convert_sizes(holdings, tuning);
    let max_hops = u8::try_from(tuning.convert.max_hops.clamp(1, 4)).unwrap_or(3);

    let mut routes: Vec<SizeRoute> = Vec::new();
    let search = |amount: u64| -> Result<Option<ptt_trade_engine::ConversionResult>, String> {
        let Ok(amount_in) = AssetAmount::from_whole_units(have.clone(), amount, &market.units)
        else {
            return Ok(None);
        };
        find_best_conversion(
            &market.index,
            &ConversionRequest {
                from_asset_id: have.clone(),
                to_asset_id: need.clone(),
                amount_in,
                max_hops,
                max_paths: 64,
                max_expansions: 10_000,
                // Every route the search ranked, not the top three. Rank is
                // not a visibility rule on this page (see `route_quotes`),
                // and a candidate the engine never hands over cannot be
                // judged on its rate at all. 50 is the engine's ceiling.
                alternative_limit: 50,
                allowed_intermediate_asset_ids: None,
                // Gross by product decision: no monetary fee is modelled.
                fee_policy: FeePolicy::None,
            },
            &SearchCancellation::default(),
        )
        .map(Some)
        .map_err(|error| format!("convert: {error:?}"))
    };

    // **The candidate list is enumerated once, at a size nobody typed.**
    //
    // The search takes the reader's holding as its input amount, and the
    // walk drops any path whose next hop rounds to zero quanta
    // (`route.rs`, `if propagated.quanta == 0`). A bridge currency dearer
    // than one unit of what they hold therefore deletes every route through
    // itself -- on the owner's real book `chaos-orb -> divine-orb` offered
    // 1 route at a 20-chaos holding and all 10 at 200, and
    // `chaos-orb -> omen-of-whittling` offered none at all under 100 chaos
    // while eight profitable rates sat in the database. That is ruling 3's
    // 错杀 arriving through the back door: not hidden for pricing badly,
    // just never enumerated.
    //
    // A route's listable rate does not depend on the ask, so the set of them
    // must not either. This enumeration size is large enough that no
    // realistic bridge rounds away, and it is only ever used to *find* the
    // paths -- every number on the page is still computed for the size the
    // reader typed, off the front rows.
    let catalogue = search(ROUTE_ENUMERATION_SIZE)?;

    for size in sizes.iter().copied() {
        let at_size = search(size)?;

        // Everything either search ranked, with the direct trade guaranteed
        // a place: it is the baseline every other row is measured against,
        // and a baseline that can fall off the bottom of `alternatives` is
        // not one. The reader's own size goes first so that when both
        // searches found a path, the one carrying this ask's fill is the one
        // the accounting below reads.
        let mut candidates: Vec<&ptt_trade_engine::ConversionPath> = Vec::new();
        let direct = at_size
            .as_ref()
            .and_then(|result| result.direct_path.as_ref())
            .or_else(|| {
                catalogue
                    .as_ref()
                    .and_then(|result| result.direct_path.as_ref())
            });
        for path in ranked_paths(&at_size)
            .chain(ranked_paths(&catalogue))
            .chain(direct)
        {
            if !candidates
                .iter()
                .any(|seen| seen.path_asset_ids == path.path_asset_ids)
            {
                candidates.push(path);
            }
        }
        if candidates.is_empty() {
            routes.push(SizeRoute {
                size,
                quotes: Vec::new(),
                direct_is_the_only_one: false,
            });
            continue;
        }
        let priced = route_quotes(&market, &candidates, direct, size);

        // The detail below belongs to the route at the top of the list the
        // reader is looking at, which is not always the one `compare_paths`
        // put first — that one may have been dropped for pricing worse than
        // direct.
        let quotes: Vec<RouteQuote> = priced.into_iter().map(|(_, quote)| quote).collect();
        routes.push(SizeRoute {
            size,
            direct_is_the_only_one: quotes.len() == 1 && quotes[0].is_direct,
            quotes,
        });
    }

    // Listing advice belongs under a page that said something. When there is
    // neither a note nor a priced size, the page reads "nothing to convert",
    // and maker advice under that heading would be advice about a pair the
    // page just said it could not price.
    let maker = (!market.notes.is_empty() || !routes.is_empty())
        .then(|| maker_model(&market, have, need, sizes[sizes.len() / 2]))
        .flatten();

    Ok(ConvertModel {
        notes: market.notes,
        have: have.clone(),
        need: need.clone(),
        sizes: routes,
        maker,
        need_structural: structural_notes_for(std::slice::from_ref(need), pulse)
            .into_iter()
            .next(),
    })
}

/// One pinch row: the step's own numbers, then what qualifies it.
///
/// `shared` carries how many routes pinch here when they all pinch at the
/// same step, and it leads the notes because it is the reason there is one
/// row instead of one per route.
fn pinch_line(language: UiLanguage, leg: &LegTakeCoverage, shared: Option<usize>) -> String {
    let text = crate::report_text::report(language);
    let facts = crate::report_text::leg_take_facts(
        language,
        leg.from_asset_id.as_str(),
        leg.to_asset_id.as_str(),
        leg,
    );
    let shared_note =
        shared.map(|count| crate::report_text::fill(text.pinch_shared, &[&count.to_string()]));
    let mut notes: Vec<&str> = Vec::new();
    if let Some(note) = shared_note.as_deref() {
        notes.push(note);
    }
    notes.extend(crate::report_text::leg_take_notes(language, leg));
    format!(
        "{facts}   {}",
        crate::report_text::join_text(language, &notes)
    )
}

/// The Convert page as display lines.
fn render_convert(model: &ConvertModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut lines = model.notes.clone();
    let (have, need) = (&model.have, &model.need);
    for route in &model.sizes {
        let size = route.size;
        if route.quotes.is_empty() {
            lines.push(format!(
                "{size:>4} {}",
                fill(text.no_route_for_pair, &[have.as_str(), need.as_str()])
            ));
            continue;
        }
        lines.push(format!("{size:>4} {have} -> {need}"));
        // When every route pinches at the same step the warning is about the
        // card, not about any one row, so it is said once above the rates
        // rather than repeated under each of them.
        let shared = route.shared_pinch();
        if let Some((leg, count)) = shared {
            lines.push(format!("       {}", pinch_line(language, leg, Some(count))));
        }
        // Rate, then what fills at it. Never the other way round: the reader
        // lists one rate and controls the quantity themselves, so the rate is
        // the decision and the quantity is the warning beside it.
        for quote in &route.quotes {
            lines.push(format!("     {}", quote_line(quote, size, have, language)));
            // One row at most, naming where the route pinches. The rate
            // line above already carries the summary; three rows repeating
            // it is how a card becomes a wall of warnings.
            if shared.is_none()
                && let Some(leg) = quote.pinch()
            {
                lines.push(format!("       {}", pinch_line(language, leg, None)));
            }
        }
        if route.direct_is_the_only_one {
            lines.push(format!("     {}", text.no_route_beats_direct));
        }
    }

    if lines.is_empty() {
        lines.push(text.nothing_to_convert.to_owned());
        return lines;
    }
    if let Some(maker) = &model.maker {
        lines.extend(render_maker(maker, have, need, language));
    }
    lines
}

/// The listing-strategy section of the Convert page: the trader's three ways
/// to act on this pair as a maker — undercut the competing front, match it,
/// or list greedily — each priced against taking the instant fill now.
///
/// Returns nothing rather than erroring: the section is advisory, and a pair
/// with no maker picture still has a working convert report above it.
fn maker_model(
    market: &Market,
    have: &MarketAssetId,
    need: &MarketAssetId,
    size: u64,
) -> Option<MakerModel> {
    use ptt_strategy::{MakerMode, MakerRequest, calculate_maker_strategy};

    let amount_in = AssetAmount::from_whole_units(have.clone(), size, &market.units).ok()?;
    let instant = market
        .index
        .fill_pair(&amount_in, need, FeePolicy::None)
        .ok()
        .flatten()
        .filter(|fill| fill.consumed_input.quanta > 0);
    let base = MakerRequest {
        from_asset_id: have,
        to_asset_id: need,
        amount_in: &amount_in,
        competing: &market.selected,
        instant: instant.as_ref(),
        match_front: false,
        thresholds: ptt_strategy::RiskThresholds::default(),
    };
    let strategy = calculate_maker_strategy(base.clone()).ok()?;
    // The match-front variant is the same Opportunity mode priced at the
    // front instead of below it; a second evaluation keeps that trade-off
    // visible without a third mode existing anywhere. Skipped with no queue,
    // where there is no front to match.
    let match_front = if strategy.queue.is_empty() {
        None
    } else {
        calculate_maker_strategy(MakerRequest {
            match_front: true,
            ..base
        })
        .ok()
        .and_then(|matched| {
            matched
                .recommendations
                .into_iter()
                .find(|item| item.mode == MakerMode::Opportunity)
        })
    };
    let shared_risks = shared_maker_risks(&strategy, match_front.as_ref());
    Some(MakerModel {
        size,
        strategy,
        match_front,
        shared_risks,
    })
}

/// The blocking risks common to every mode the panel will draw.
///
/// Intersected over exactly the rows that get drawn — the strategy's own
/// recommendations plus the match-front evaluation — and nothing else.
/// Hoisting a risk that no drawn row carries would invent a warning about the
/// pair out of a hazard the reader was never shown.
fn shared_maker_risks(
    strategy: &ptt_strategy::MakerStrategy,
    match_front: Option<&ptt_strategy::MakerRecommendation>,
) -> Vec<ptt_strategy::ExecutionRisk> {
    let mut rows = strategy
        .recommendations
        .iter()
        .chain(match_front)
        .map(|item| item.assessment.blocking());
    let Some(first) = rows.next() else {
        return Vec::new();
    };
    // Filtering only ever removes, so the header list and every remainder
    // keep the one canonical order `blocking()` hands out.
    rows.fold(first, |common, next| {
        common
            .into_iter()
            .filter(|risk| next.contains(risk))
            .collect()
    })
}

/// The listing-strategy section as display lines.
fn render_maker(
    model: &MakerModel,
    have: &MarketAssetId,
    need: &MarketAssetId,
    language: UiLanguage,
) -> Vec<String> {
    use crate::report_text::fill;
    use ptt_strategy::{MakerMode, MakerRecommendation};

    let text = crate::report_text::report(language);
    let strategy = &model.strategy;
    let size = model.size;

    let mut lines = vec![fill(
        text.maker_header,
        &[have.as_str(), need.as_str(), &size.to_string()],
    )];
    match &strategy.instant_rate {
        Some(rate) => lines.push(format!("     {}", fill(text.maker_instant, &[&rate.text]))),
        None => lines.push(format!("     {}", text.maker_no_instant)),
    }
    if strategy.queue.is_empty() {
        lines.push(format!("     {}", text.maker_no_book));
        return lines;
    }
    // The book's own hazards hold whichever way you price inside it, so the
    // panel states them once instead of once per mode.
    if !model.shared_risks.is_empty() {
        lines.push(format!(
            "     {} {}",
            text.maker_risks,
            crate::report_text::join(
                language,
                &model.shared_risks,
                crate::report_text::execution_risk
            )
        ));
    }

    let mode_line = |template: &str, recommendation: &MakerRecommendation| -> Vec<String> {
        let mut block = vec![format!(
            "     {}",
            fill(template, &[&recommendation.rate.text])
        )];
        let verdict = if recommendation.beats_instant {
            match (
                &recommendation.improvement_over_instant,
                recommendation.improvement_basis_points,
            ) {
                (Some(delta), Some(points)) => Some(fill(
                    text.maker_improvement,
                    &[
                        &delta.quanta.to_string(),
                        need.as_str(),
                        &crate::report_text::percent_from_basis_points(points),
                    ],
                )),
                _ => None,
            }
        } else {
            Some(text.maker_not_worth.to_owned())
        };
        if let Some(verdict) = verdict {
            block.push(format!("        {verdict}"));
        }
        // Only what this mode adds; the rest is on the panel's own line.
        let blocking = model.mode_only_risks(recommendation);
        if !blocking.is_empty() {
            block.push(format!(
                "        {} {}",
                risks_label(language),
                crate::report_text::join(language, &blocking, crate::report_text::execution_risk)
            ));
        }
        block
    };

    let undercut = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Opportunity);
    if let Some(recommendation) = undercut {
        lines.extend(mode_line(text.maker_undercut, recommendation));
    }
    if let Some(recommendation) = &model.match_front {
        lines.extend(mode_line(text.maker_match, recommendation));
    }
    if let Some(recommendation) = strategy
        .recommendations
        .iter()
        .find(|item| item.mode == MakerMode::Greedy)
    {
        lines.extend(mode_line(text.maker_greedy, recommendation));
    }

    if let Some(spread) = strategy.spread_basis_points {
        lines.push(format!(
            "     {}",
            fill(
                text.maker_spread,
                &[&crate::report_text::percent_from_basis_points(spread)]
            )
        ));
    }
    if let (Some(depth), Some(cap)) = (
        &strategy.visible_depth_from,
        &strategy.suggested_max_single_order,
    ) {
        lines.push(format!(
            "     {}",
            crate::report_text::join_text(
                language,
                &[
                    &fill(
                        text.maker_depth,
                        &[&depth.quanta.to_string(), have.as_str()],
                    ),
                    &fill(
                        text.maker_max_single,
                        &[&cap.quanta.to_string(), have.as_str()],
                    ),
                ],
            )
        ));
    }
    for excluded in &strategy.excluded {
        lines.push(format!(
            "     {}",
            fill(
                text.maker_excluded,
                &[
                    &excluded.rate.text,
                    &excluded.stock.to_string(),
                    crate::report_text::maker_exclusion(language, excluded.reason),
                ],
            )
        ));
    }
    lines
}

/// The market policy the settings ask for: the configured settlement
/// currencies become the core-liquidity set, with the shipped default as the
/// fallback and a visible line — never a silent one — when the configuration
/// could not be used.
fn market_policy_from(
    tuning: &MarketTuning,
    league: &str,
    language: UiLanguage,
) -> (MarketPolicy, Option<String>) {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut policy = MarketPolicy::default_for(league);
    if tuning.settlement_assets.is_empty() {
        return (policy, None);
    }
    let parsed: Vec<MarketAssetId> = tuning
        .settlement_assets
        .iter()
        .filter_map(|id| MarketAssetId::try_new(id).ok())
        .collect();
    let dropped = tuning.settlement_assets.len() - parsed.len();
    if parsed.is_empty() || !policy.set_core_liquidity(parsed) {
        return (policy, Some(text.settlement_config_invalid.to_owned()));
    }
    let warning =
        (dropped > 0).then(|| fill(text.settlement_config_partial, &[&dropped.to_string()]));
    (policy, warning)
}

/// Currencies with real buy pressure the user has not put in the focus list.
///
/// The window's books are folded as one pseudo-day and run through the
/// market pulse, so the evidence is anchor-valued listed quantities on both
/// sides — never how often a book was captured. An asset the user flips all
/// day with nothing on its demand side is not suggested; an asset seen once
/// with heavy buy pressure is. The pulse's quiet floor is the offer bar: no
/// direction can be read below it.
///
/// Offered from the first session rather than only once a list exists. The
/// list is meant to be built out of what the market turns out to want, and a
/// feature that suggests nothing until you have already used it cannot start
/// that.
fn focus_suggestions(
    observations: &[MarketEdgeObservation],
    policy: &MarketPolicy,
    tuning: &MarketTuning,
) -> Vec<FocusSuggestion> {
    let configured: std::collections::BTreeSet<MarketAssetId> = tuning
        .focus_assets
        .iter()
        .chain(&tuning.bridge_assets)
        .chain(&tuning.watch_only_assets)
        .filter_map(|id| MarketAssetId::try_new(id).ok())
        .collect();
    let day_key = Utc::now().format("%Y-%m-%d").to_string();
    let stats = crate::rollup::fold_window_stats(
        observations,
        &day_key,
        tuning.risk.top_book_outlier_factor,
    );
    let thresholds = analytics_thresholds_from(tuning).unwrap_or_default();
    let pulse = ptt_strategy::market_pulse(&stats, &policy.core_liquidity, &thresholds);

    let mut suggestions = Vec::new();
    // pulse.assets arrive sorted by demand pressure descending.
    for asset in &pulse.assets {
        if policy.is_core_liquidity(&asset.asset_id) || configured.contains(&asset.asset_id) {
            continue;
        }
        // Unvalued demand cannot be compared across assets; below the quiet
        // floor there is no direction to read at all.
        let Some(demand) = asset.demand_anchor else {
            continue;
        };
        if demand < thresholds.quiet_floor_anchor_units {
            continue;
        }
        let demand = u64::try_from(demand).unwrap_or(u64::MAX);
        // Dismissals are held until the pressure doubles what it was.
        let due = tuning
            .ignored_suggestions
            .iter()
            .find(|ignored| ignored.asset_id == asset.asset_id.as_str())
            .is_none_or(|ignored| ignored.is_due_again(demand));
        if !due {
            continue;
        }
        suggestions.push(FocusSuggestion {
            asset_id: asset.asset_id.clone(),
            demand_anchor: demand,
            supply_anchor: u64::try_from(asset.supply_anchor.unwrap_or(0)).unwrap_or(u64::MAX),
        });
    }
    suggestions.truncate(4);
    suggestions
}

/// What a scope is being built to answer.
///
/// The two answers differ, and that difference is the point of having a focus
/// list at all. Arbitrage is arithmetic: a currency the window can price is
/// worth checking whether or not anyone declared an interest in it, and
/// refusing to price it because it is off a list throws away a real
/// opportunity the data already contains. Attention is a choice: coverage and
/// probe suggestions measure what the user says they trade, and measuring
/// them against everything the watcher happened to see buries that answer
/// under currencies nobody asked about.
///
/// Conflating them meant the moment a user curated a list — the feature
/// working as intended — everything they had captured but not listed silently
/// stopped being scanned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusPurpose {
    /// The list, and everything else the window can price.
    Arbitrage,
    /// The list. Or, with no list configured, everything seen: a scope of
    /// nothing would report perfect coverage of no pairs.
    Attention,
}

/// The focus items the reports scope over: the settlement currencies as
/// anchors, then the user's configured lists, then — for arbitrage — every
/// other asset seen in the window.
///
/// Watch-only and bridge are pushed first and win over target, so a currency
/// the user has explicitly demoted cannot come back in through "seen". That
/// is the one exclusion a focus list still performs, and it is deliberate:
/// watch-only means "price it, never route through it".
///
/// Listed targets come before seen ones because the radar divides its budget
/// among targets in order, so the list decides who gets scanned first when
/// there is not enough budget for everyone.
fn focus_items_from(
    policy: &MarketPolicy,
    tuning: &MarketTuning,
    seen: &[MarketAssetId],
    purpose: FocusPurpose,
) -> Vec<FocusGroupItem> {
    let mut taken = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    let push = |asset: MarketAssetId,
                role: FocusRole,
                taken: &mut std::collections::BTreeSet<MarketAssetId>,
                items: &mut Vec<FocusGroupItem>| {
        if taken.insert(asset.clone()) {
            items.push(FocusGroupItem {
                asset_id: asset,
                role,
            });
        }
    };
    for asset in &policy.core_liquidity {
        push(asset.clone(), FocusRole::Anchor, &mut taken, &mut items);
    }
    for (list, role) in [
        (&tuning.watch_only_assets, FocusRole::WatchOnly),
        (&tuning.bridge_assets, FocusRole::Bridge),
    ] {
        for id in list {
            if let Ok(asset) = MarketAssetId::try_new(id) {
                push(asset, role, &mut taken, &mut items);
            }
        }
    }
    for id in &tuning.focus_assets {
        if let Ok(asset) = MarketAssetId::try_new(id) {
            push(asset, FocusRole::Target, &mut taken, &mut items);
        }
    }
    let include_seen = purpose == FocusPurpose::Arbitrage || tuning.focus_assets.is_empty();
    if include_seen {
        for asset in seen {
            push(asset.clone(), FocusRole::Target, &mut taken, &mut items);
        }
    }
    items
}

/// "Is what I am watching healthy": coverage, valuations, and what to go
/// look at next.
pub fn watchlist_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = watchlist_model(observations, context_key, league, tuning, language, None)?;
    Ok(render_watchlist(&model, language))
}

/// Everything the Watchlist page knows, before anything decides how to draw
/// it.
pub fn watchlist_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Result<WatchlistModel, String> {
    let market = build_market(observations, context_key, tuning, language)?;
    let (policy, policy_warning) = market_policy_from(tuning, league, language);
    let mut notes = market.notes.clone();
    notes.extend(policy_warning);

    // Value everything seen against the first core currency.
    let Some(anchor) = policy.core_liquidity.first().cloned() else {
        return Ok(WatchlistModel {
            notes,
            core_liquidity: policy.core_liquidity,
            valuations: Vec::new(),
            coverage: CoverageOutcome::NotComputed,
            suggestions: Vec::new(),
            anchors: Vec::new(),
        });
    };
    let mut seen: Vec<MarketAssetId> = observations
        .iter()
        .flat_map(|observation| {
            [
                observation.edge.from_asset_id.clone(),
                observation.edge.to_asset_id.clone(),
            ]
        })
        .collect();
    seen.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    seen.dedup();

    let valuations = seen
        .iter()
        .filter(|asset| **asset != anchor)
        .map(|asset| AssetValuation {
            asset_id: asset.clone(),
            valuation: value_against_anchor(ValuationRequest {
                asset_id: asset,
                anchor_asset_id: &anchor,
                mode: ValuationMode::Midpoint,
                edges: &market.selected,
                include_historical: false,
            }),
        })
        .collect();

    // Typed coverage gaps for the pairs this focus group cares about, and
    // the probes that would close them. Gaps touching scarce or high-turnover
    // currencies jump the queue.
    let coverage = match focus_coverage(
        observations,
        context_key,
        &policy,
        tuning,
        &seen,
        Some(&market),
    ) {
        Ok((status, entries, mut candidates)) => {
            boost_probe_candidates(&mut candidates, pulse);
            CoverageOutcome::Ready(CoverageModel {
                status,
                entries,
                candidates,
            })
        }
        Err(reason) => CoverageOutcome::Failed(reason),
    };

    let suggestions = focus_suggestions(observations, &policy, tuning);
    let anchors = recommend_liquidity_anchors(&market.selected, &policy);

    Ok(WatchlistModel {
        notes,
        core_liquidity: policy.core_liquidity,
        valuations,
        coverage,
        suggestions,
        anchors,
    })
}

/// The Watchlist page as display lines.
fn render_watchlist(model: &WatchlistModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut lines = model.notes.clone();

    lines.push(fill(
        text.core_liquidity,
        &[&model
            .core_liquidity
            .iter()
            .map(MarketAssetId::as_str)
            .collect::<Vec<_>>()
            .join(", ")],
    ));

    for entry in &model.valuations {
        let value = match (&entry.valuation.value, entry.valuation.status) {
            (Some(value), ValuationStatus::TwoSided) => {
                fill(text.valuation_two_sided, &[&value.text])
            }
            (Some(value), _) => fill(text.valuation_one_sided, &[&value.text]),
            (None, _) => text.no_price_capture.to_owned(),
        };
        lines.push(format!("{:<20} {value}", entry.asset_id.as_str(),));
    }

    match &model.coverage {
        CoverageOutcome::NotComputed => return lines,
        CoverageOutcome::Failed(reason) => {
            lines.push(fill(text.coverage_unavailable, &[reason]));
        }
        CoverageOutcome::Ready(coverage) => lines.extend(render_coverage(coverage, language)),
    }

    for suggestion in &model.suggestions {
        lines.push(fill(
            text.focus_suggestion,
            &[
                suggestion.asset_id.as_str(),
                &suggestion.demand_anchor.to_string(),
                &suggestion.supply_anchor.to_string(),
            ],
        ));
    }

    for recommendation in &model.anchors {
        lines.push(fill(
            text.anchor_recommendation,
            &[
                crate::report_text::anchor_action(language, recommendation.action),
                recommendation.asset_id.as_str(),
                &format!(
                    "{}.{}",
                    recommendation.score_tenths / 10,
                    recommendation.score_tenths % 10
                ),
                &recommendation.pair_coverage_count.to_string(),
                &recommendation.bidirectional_pair_count.to_string(),
            ],
        ));
    }
    lines
}

/// "Where is the money right now": the unified radar.
///
/// This is the page the whole loop points at. Everything else answers a
/// question the user had to think of first — this one ranks what the book
/// already knows, so the answer arrives before the question.
///
/// Anchored on the league's first core currency: a radar has to start
/// somewhere, and the currency everything else is quoted in is the one holding
/// that is not itself a position. Items keep their execution category, because
/// "executable now" and "someone would have to take this listing" are
/// different products and collapsing them is how a theoretical number gets
/// traded.
pub fn opportunities_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = opportunities_model(observations, context_key, league, tuning, language, None)?;
    Ok(render_opportunities(&model, language))
}

/// Everything the Opportunities page knows, before anything decides how to
/// draw it.
pub fn opportunities_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Result<OpportunitiesModel, String> {
    let (policy, policy_warning) = market_policy_from(tuning, league, language);
    // The unavailable answers carry no notes: each is a single sentence about
    // why there is no page, and a degradation notice above it would be
    // commentary on a scan that never ran.
    if policy.core_liquidity.is_empty() {
        return Ok(OpportunitiesModel {
            notes: Vec::new(),
            scan: RadarScan::Unavailable(RadarUnavailable::NoCoreCurrency),
        });
    }
    // Counted before the book is built: an empty window has no assets, and
    // `build_market` cannot make a unit catalogue out of none.
    let mut seen: Vec<MarketAssetId> = observations
        .iter()
        .flat_map(|observation| {
            [
                observation.edge.from_asset_id.clone(),
                observation.edge.to_asset_id.clone(),
            ]
        })
        .collect();
    seen.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    seen.dedup();
    if seen.len() < 2 {
        return Ok(OpportunitiesModel {
            notes: Vec::new(),
            scan: RadarScan::Unavailable(RadarUnavailable::NotEnoughMarket),
        });
    }
    let market = build_market(observations, context_key, tuning, language)?;

    let items = focus_items_from(&policy, tuning, &seen, FocusPurpose::Arbitrage);
    // Routing is the one place the setting applies. Coverage keeps the plain
    // policy below: target-to-target pairs would square the list the reader
    // is meant to work through, and that list is about attention.
    let scope = FocusScope::try_new(
        &items,
        FocusScopePolicy {
            allow_target_interconnect: tuning.route_through_targets,
            ..FocusScopePolicy::default()
        },
    )
    .map_err(|error| format!("scope: {error}"))?;

    // One start per settlement currency the book can actually stake — a
    // configured settlement asset the window has never seen has no unit yet
    // and is skipped rather than failing the whole scan.
    //
    // The scan runs at the same canonical size the Convert page enumerates
    // at, never at anything the reader holds (their ruling: the radar finds
    // rates, the reader brings the size in the detail panel). The amount
    // exists only because the engine needs one to walk with.
    let mut starts = Vec::new();
    for asset in &policy.core_liquidity {
        if let Ok(amount_in) =
            AssetAmount::from_whole_units(asset.clone(), ROUTE_ENUMERATION_SIZE, &market.units)
        {
            starts.push(RadarStart {
                start_asset_id: asset.clone(),
                amount_in,
            });
        }
    }
    if starts.is_empty() {
        return Ok(OpportunitiesModel {
            notes: Vec::new(),
            scan: RadarScan::Unavailable(RadarUnavailable::NoStartUnits {
                anchor: policy.core_liquidity.first().cloned(),
            }),
        });
    }
    let start_names: Vec<MarketAssetId> = starts
        .iter()
        .map(|start| start.start_asset_id.clone())
        .collect();
    // Settings values pass through the engine's own validation bounds; a
    // hand-edited extreme is clamped to the largest honest value rather than
    // failing the page.
    let minimum_bps =
        u32::try_from(tuning.radar.minimum_profit_basis_points.min(1_000_000)).unwrap_or(100);
    let budget_expansions =
        u32::try_from(tuning.radar.max_total_expansions.clamp(1, 1_000_000)).unwrap_or(60_000);
    let max_results = u16::try_from(tuning.radar.max_results.clamp(1, 500)).unwrap_or(12);
    // Three is the shortest loop that exists; past that the graph limits the
    // walk long before the bound does.
    let max_cycle_length = u8::try_from(tuning.radar.max_cycle_length.clamp(3, 12)).unwrap_or(6);
    let request = RadarRequest {
        context_key: context_key.to_owned(),
        starts,
        minimum_conversion_improvement_basis_points: minimum_bps,
        minimum_triangle_profit_basis_points: minimum_bps,
        max_hops: 3,
        max_cycle_length,
        max_paths_per_target: 32,
        max_expansions_per_target: 4_000,
        budget: RadarBudget {
            max_total_expansions: budget_expansions,
            max_targets: 48,
        },
        max_triangle_evaluations: 4_000,
        max_results,
        // Gross by product decision: no monetary fee is modelled.
        fee_policy: FeePolicy::None,
        thresholds: risk_thresholds_from(tuning, pulse),
    };
    let result = run_opportunity_radar(
        &market.instant_selection,
        &market.units,
        &scope,
        &request,
        &SearchCancellation::default(),
        |_| {},
    )
    .map_err(|error| format!("radar: {error:?}"))?;

    let mut notes = market.notes.clone();
    notes.extend(policy_warning);

    let freshness = market.instant_selection.policy.freshness;
    let now = Utc::now();
    let items = result
        .items
        .into_iter()
        .map(|item| {
            // The light reads the oldest leg: a route is only as current as
            // the capture it leans on hardest.
            let light = item
                .conversion_path
                .as_ref()
                .and_then(|path| path.capture_time_evidence.as_ref())
                .or_else(|| {
                    item.triangle
                        .as_ref()
                        .and_then(|triangle| triangle.capture_time_evidence.as_ref())
                })
                .map(|evidence| {
                    freshness
                        .classify(evidence.earliest_captured_at, now)
                        .status
                });
            let structural = structural_notes_for(&item.path_asset_ids, pulse);
            let leg_books = item
                .conversion_path
                .as_ref()
                .map(|path| path.steps.as_slice())
                .or_else(|| {
                    item.triangle
                        .as_ref()
                        .map(|triangle| triangle.steps.as_slice())
                })
                .map(|steps| route_leg_books(&market, steps))
                .unwrap_or_default();
            OpportunityRow {
                item,
                light,
                structural,
                leg_books,
            }
        })
        .collect();

    let mut probe_candidates = result.probe_candidates;
    boost_probe_candidates(&mut probe_candidates, pulse);

    Ok(OpportunitiesModel {
        notes,
        scan: RadarScan::Ran(Box::new(RadarScanResult {
            starts: start_names,
            items,
            probe_candidates,
            diagnostics: result.diagnostics,
        })),
    })
}

/// The Opportunities page as display lines.
fn render_opportunities(model: &OpportunitiesModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);

    let scan = match &model.scan {
        RadarScan::Unavailable(RadarUnavailable::NoCoreCurrency) => {
            return vec![text.no_core_currency.to_owned()];
        }
        RadarScan::Unavailable(RadarUnavailable::NotEnoughMarket) => {
            return vec![text.not_enough_market.to_owned()];
        }
        RadarScan::Unavailable(RadarUnavailable::NoStartUnits { anchor }) => {
            return vec![fill(
                text.no_start_units,
                &[anchor.as_ref().map_or("?", MarketAssetId::as_str)],
            )];
        }
        RadarScan::Ran(scan) => scan,
    };

    let mut lines = model.notes.clone();
    lines.push(fill(
        text.scanning_from,
        &[
            &scan
                .starts
                .iter()
                .map(MarketAssetId::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            &scan.diagnostics.target_count.to_string(),
        ],
    ));
    // Said before the results, not after: a truncated search that looks like a
    // complete one is how "there is nothing better" gets believed.
    if scan.diagnostics.budget_exhausted || scan.diagnostics.results_truncated {
        lines.push(fill(
            text.partial_scan,
            &[
                &scan.diagnostics.skipped_target_count.to_string(),
                &scan.diagnostics.expansions_used.to_string(),
                if scan.diagnostics.results_truncated {
                    text.results_cut
                } else {
                    ""
                },
            ],
        ));
    }
    // The radar's own probe suggestions — the pairs whose absence or
    // staleness limited what it could claim. Shown whether or not any item
    // survived, because an empty page is exactly when "go flip X" matters
    // most.
    let mut probe_lines = Vec::new();
    if !scan.probe_candidates.is_empty() {
        probe_lines.push(text.radar_probe_header.to_owned());
        for candidate in scan.probe_candidates.iter().take(4) {
            probe_lines.push(format!(
                "  {}  {} {} -> {}   ({})",
                crate::report_text::probe_priority(language, candidate.priority),
                text.flip,
                candidate.from_asset_id.as_str(),
                candidate.to_asset_id.as_str(),
                crate::report_text::probe_reason(language, candidate.reason),
            ));
        }
    }
    if scan.items.is_empty() {
        lines.push(text.nothing_beats_holding.to_owned());
        if scan.diagnostics.missing_conversion_count > 0 {
            lines.push(fill(
                text.targets_without_route,
                &[&scan.diagnostics.missing_conversion_count.to_string()],
            ));
        }
        lines.extend(probe_lines);
        return lines;
    }

    for row in &scan.items {
        lines.extend(radar_item_lines(
            &row.item,
            walk_route(&row.leg_books, 1).rate,
            row.light,
            language,
        ));
    }
    lines.extend(probe_lines);
    lines
}

/// One radar item, as the page prints it.
///
/// Split out so it can be tested against a hand-built item. Reaching this code
/// through the search needs a market that actually contains an arbitrage, and
/// the captured corpus does not have one — leaving the only branch a user sees
/// when the radar succeeds as the only branch never executed.
/// The word introducing a list of risk flags.
const fn risks_label(language: UiLanguage) -> &'static str {
    crate::report_text::report(language).risks
}

fn radar_item_lines(
    item: &ptt_workflows::RadarItem,
    rate: Option<RouteRate>,
    freshness: Option<FreshnessStatus>,
    language: UiLanguage,
) -> Vec<String> {
    let text = crate::report_text::report(language);
    let route = item
        .path_asset_ids
        .iter()
        .map(MarketAssetId::as_str)
        .collect::<Vec<_>>()
        .join(" -> ");
    let edge = item.value_basis_points.map_or_else(
        || text.unpriced.to_owned(),
        crate::report_text::percent_from_basis_points,
    );
    let category = crate::report_text::actionability(language, item.category);
    let light = freshness.map_or(String::new(), |status| {
        format!(
            "   {}",
            crate::report_text::freshness_light(language, status)
        )
    });
    // The composed front rate where the walked amount used to be: the scan
    // runs at a canonical size nobody holds, so its output is not a number
    // about the reader — the rate is, and it is the same one the detail
    // panel's walk prices their own ask at.
    let rate = rate.map_or_else(|| text.unpriced.to_owned(), RouteRate::text);
    let mut lines = vec![
        format!(
            "{edge:>8}  {}  {route}",
            crate::report_text::radar_item_kind(language, item.kind)
        ),
        format!("          {category}   {rate}{light}"),
    ];
    if !item.blocking_risks.is_empty() {
        lines.push(format!(
            "          {} {}",
            risks_label(language),
            crate::report_text::join(
                language,
                &item.blocking_risks,
                crate::report_text::execution_risk
            )
        ));
    }
    if !item.reasons.is_empty() {
        lines.push(format!(
            "          {}",
            crate::report_text::join(language, &item.reasons, crate::report_text::radar_reason)
        ));
    }
    lines
}

/// "What should I go look at next": the probe queue on its own.
///
/// This is the loop the product is built around — a gap in the book becomes a
/// suggestion, the suggestion sends the user to that pair in game, the watcher
/// ingests it, and the suggestion disappears. Monitor shows it permanently so
/// the next move is always on screen.
pub fn probe_queue(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = probe_queue_model(observations, context_key, league, tuning, language, None)?;
    Ok(render_probe_queue(&model, language))
}

/// The probe queue, before anything decides how to draw it.
pub fn probe_queue_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> Result<ProbeQueueModel, String> {
    let (policy, _policy_warning) = market_policy_from(tuning, league, language);
    let mut seen: Vec<MarketAssetId> = observations
        .iter()
        .flat_map(|observation| {
            [
                observation.edge.from_asset_id.clone(),
                observation.edge.to_asset_id.clone(),
            ]
        })
        .collect();
    seen.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    seen.dedup();
    if seen.is_empty() {
        return Ok(ProbeQueueModel {
            nothing_captured: true,
            complete_pairs: 0,
            total_pairs: 0,
            candidates: Vec::new(),
        });
    }

    let (_status, coverage, mut candidates) =
        focus_coverage(observations, context_key, &policy, tuning, &seen, None)?;
    boost_probe_candidates(&mut candidates, pulse);
    let missing = coverage
        .iter()
        .filter(|entry| entry.status != ptt_workflows::FocusCoverageStatus::Complete)
        .count();
    Ok(ProbeQueueModel {
        nothing_captured: false,
        complete_pairs: coverage.len() - missing,
        total_pairs: coverage.len(),
        candidates,
    })
}

/// The probe queue as display lines.
fn render_probe_queue(model: &ProbeQueueModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    if model.nothing_captured {
        return vec![text.no_pairs_captured.to_owned()];
    }
    let mut lines = vec![fill(
        text.pairs_complete,
        &[
            &model.complete_pairs.to_string(),
            &model.total_pairs.to_string(),
        ],
    )];
    if model.candidates.is_empty() {
        lines.push(text.nothing_to_probe.to_owned());
        return lines;
    }
    for candidate in model.candidates.iter().take(6) {
        lines.push(format!(
            "{}  flip {} -> {}   ({})",
            crate::report_text::probe_priority(language, candidate.priority),
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            crate::report_text::probe_reason(language, candidate.reason),
        ));
    }
    lines
}

/// "What has this pair been doing": candles, a summary, and what looks off.
pub fn history_report(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<Vec<String>, String> {
    let model = history_model(observations, context_key, have, need, tuning, language)?;
    Ok(render_history(&model, language))
}

/// Everything the History page knows, before anything decides how to draw it.
pub fn history_model(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    have: &MarketAssetId,
    need: &MarketAssetId,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> Result<HistoryModel, String> {
    let market = build_market(observations, context_key, tuning, language)?;
    let points = price_points(&market.selected, have, need);
    if points.is_empty() {
        return Ok(HistoryModel {
            notes: market.notes,
            have: have.clone(),
            need: need.clone(),
            summary: None,
            light: None,
            candles: Vec::new(),
            anomalies: Vec::new(),
        });
    }

    let summary = summarize(&points, have, need);
    // The pair's traffic light reads the newest capture of this direction:
    // older points are history, but the light describes what acting now
    // would lean on.
    let light = observations
        .iter()
        .filter(|observation| {
            observation.edge.from_asset_id == *have && observation.edge.to_asset_id == *need
        })
        .map(|observation| observation.edge.captured_at)
        .max()
        .map(|newest| {
            market
                .instant_selection
                .policy
                .freshness
                .classify(newest, Utc::now())
                .status
        });
    let anomalies = anomalies(&summary, &points, &anomaly_thresholds_from(tuning));
    Ok(HistoryModel {
        notes: market.notes,
        have: have.clone(),
        need: need.clone(),
        candles: candles(&points, BucketSize::FiveMinutes),
        summary: Some(summary),
        light,
        anomalies,
    })
}

/// The History page as display lines.
fn render_history(model: &HistoryModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let (have, need) = (&model.have, &model.need);
    let mut lines = model.notes.clone();

    let Some(summary) = &model.summary else {
        lines.push(fill(text.no_history_yet, &[have.as_str(), need.as_str()]));
        return lines;
    };
    lines.push(fill(
        text.history_header,
        &[
            have.as_str(),
            need.as_str(),
            &summary.point_count.to_string(),
            &summary.snapshot_count.to_string(),
        ],
    ));
    if let Some(status) = model.light {
        lines.push(fill(
            text.freshness_light_line,
            &[crate::report_text::freshness_light(language, status)],
        ));
    }
    if let Some(median) = &summary.median_rate {
        lines.push(fill(
            text.median_low_high,
            &[
                &median.text,
                summary
                    .min_rate
                    .as_ref()
                    .map_or("—", |rate| rate.text.as_str()),
                summary
                    .max_rate
                    .as_ref()
                    .map_or("—", |rate| rate.text.as_str()),
            ],
        ));
    }
    if let Some(spread) = summary.spread_basis_points {
        lines.push(fill(
            text.maker_over_taker,
            &[&crate::report_text::percent_from_basis_points(spread)],
        ));
    }
    if summary.historical_only {
        lines.push(text.nothing_current.to_owned());
    }

    // Newest first, and only the most recent few: the model keeps every
    // candle so a chart can draw the whole window.
    for candle in model.candles.iter().rev().take(8) {
        lines.push(fill(
            text.candle_line,
            &[
                &candle.bucket_start.format("%H:%M").to_string(),
                &candle.open.text,
                &candle.high.text,
                &candle.low.text,
                &candle.close.text,
                &candle.sample_count.to_string(),
                if candle.maker_only {
                    text.listings_note
                } else {
                    ""
                },
            ],
        ));
    }

    for anomaly in &model.anomalies {
        lines.push(format!(
            "{} ({}){}",
            crate::report_text::price_anomaly_kind(language, anomaly.kind),
            crate::report_text::anomaly_severity(language, anomaly.severity),
            anomaly.basis_points.map_or_else(String::new, |bps| {
                format!(" {}", crate::report_text::percent_from_basis_points(bps))
            }),
        ));
    }
    lines
}

/// Coverage and probe queue for the current focus group.
///
/// Coverage needs three views of the same book — what can be taken now, what
/// is only listed, and what shows up once old data is allowed — because
/// "missing" and "stale" are different problems with different fixes.
/// Prints every candidate edge for one pair in all three coverage views.
///
/// Diagnosis, not product: "I captured this pair and coverage still calls it
/// missing" cannot be answered from the screen, because the screen shows the
/// verdict and the reasons live on the edges. This walks the same three
/// selections coverage walks and says what each saw.
pub fn debug_pair(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    tuning: &MarketTuning,
    from: &str,
    to: &str,
) -> Result<(), String> {
    let market = build_market(observations, context_key, tuning, UiLanguage::English)?;
    let now = Utc::now();
    let mut views = vec![("instant", market.instant_selection.clone())];
    for (name, strategy) in [
        ("maker", QuoteSelectionStrategy::FastMaker),
        ("probe", QuoteSelectionStrategy::Probe),
    ] {
        let (policy, _) = selection_policy_from(tuning, strategy, UiLanguage::English)?;
        views.push((
            name,
            select_quote_edges(&market.book, &policy, now)
                .map_err(|error| format!("select: {error}"))?,
        ));
    }
    for (name, view) in &views {
        let selection = view.selections.iter().find(|selection| {
            selection.from_asset_id.as_str() == from && selection.to_asset_id.as_str() == to
        });
        let Some(selection) = selection else {
            println!("[{name}] no selection entry for {from} -> {to}");
            continue;
        };
        println!(
            "[{name}] {from} -> {to}: {} candidate edge(s), selected={}",
            selection.candidate_edges.len(),
            selection.selected_edge.is_some(),
        );
        for edge in &selection.candidate_edges {
            println!(
                "  role={:?} exec={:?} rate={} stock={} captured={} fresh={:?}",
                edge.observation.edge.role,
                edge.observation.edge.execution_type,
                edge.observation.edge.rate.text,
                edge.observation.edge.stock,
                edge.observation.edge.captured_at.format("%H:%M:%S"),
                edge.freshness.status,
            );
            println!(
                "    accepted={} depth_eligible={} rejections={:?} blockers={:?} risks={:?}",
                edge.accepted_for_selection,
                edge.eligible_for_depth_analysis,
                edge.selection_rejections,
                edge.execution_blockers,
                edge.risk_flags,
            );
        }
    }
    Ok(())
}

/// Why the radar says a target is unreachable, against the live database.
///
/// Diagnosis for "the radar wants a quote I have already captured": walks the
/// same scope and starts `opportunities_model` builds, and for each target
/// prints which step refuses it — no unit, no scope edge, no selected direct
/// edge, or no path. The one number that matters is how many of these the
/// selection could have answered directly.
pub fn debug_radar(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    league: &str,
    tuning: &MarketTuning,
) -> Result<(), String> {
    let (policy, _) = market_policy_from(tuning, league, UiLanguage::English);
    let mut seen: Vec<MarketAssetId> = observations
        .iter()
        .flat_map(|observation| {
            [
                observation.edge.from_asset_id.clone(),
                observation.edge.to_asset_id.clone(),
            ]
        })
        .collect();
    seen.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    seen.dedup();
    let market = build_market(observations, context_key, tuning, UiLanguage::English)?;
    let items = focus_items_from(&policy, tuning, &seen, FocusPurpose::Arbitrage);
    let scope = FocusScope::try_new(
        &items,
        FocusScopePolicy {
            allow_target_interconnect: tuning.route_through_targets,
            ..FocusScopePolicy::default()
        },
    )
    .map_err(|error| format!("scope: {error}"))?;
    println!(
        "scope: status={:?} anchors={} targets={} bridges={} watch_only={} intermediates={} \
         endpoints={} route_through_targets={}",
        scope.status,
        scope.anchors.len(),
        scope.targets.len(),
        scope.bridges.len(),
        scope.watch_only.len(),
        scope.intermediate_asset_ids.len(),
        scope.endpoint_asset_ids.len(),
        tuning.route_through_targets,
    );
    let selection = &market.instant_selection;
    println!(
        "instant selection: {} pair entries, {} with a selected edge",
        selection.selections.len(),
        selection
            .selections
            .iter()
            .filter(|entry| entry.selected_edge.is_some())
            .count(),
    );
    let kept = selection
        .selections
        .iter()
        .filter(|entry| scope.edge_allowed(&entry.from_asset_id, &entry.to_asset_id))
        .count();
    println!(
        "after scope restriction: {kept} of {} pair entries survive",
        selection.selections.len()
    );

    for start in &policy.core_liquidity {
        for target in &scope.endpoint_asset_ids {
            if target == start || !scope.endpoint_pair_allowed(start, target) {
                continue;
            }
            let direct = selection
                .selections
                .iter()
                .find(|entry| entry.from_asset_id == *start && entry.to_asset_id == *target);
            let direct_state = match direct {
                None => "no-entry".to_owned(),
                Some(entry) => format!(
                    "selected={} candidates={} scope_allows={}",
                    entry.selected_edge.is_some(),
                    entry.candidate_edges.len(),
                    scope.edge_allowed(&entry.from_asset_id, &entry.to_asset_id),
                ),
            };
            // What the radar actually asks the engine, at several sizes: a
            // path that appears only at a larger size was never a data gap.
            // The first entry is the canonical scan size the radar itself
            // uses now that it no longer assumes a stake.
            let mut routed = String::new();
            for stake in [ROUTE_ENUMERATION_SIZE, 100, 10_000] {
                let Ok(amount_in) =
                    AssetAmount::from_whole_units(start.clone(), stake, &market.units)
                else {
                    continue;
                };
                let restricted = {
                    let mut clone = selection.clone();
                    clone.selections.retain(|entry| {
                        scope.edge_allowed(&entry.from_asset_id, &entry.to_asset_id)
                    });
                    clone
                };
                let Ok(index) =
                    MarketDepthIndex::try_from_selection(&restricted, market.units.clone())
                else {
                    continue;
                };
                let routable: Vec<MarketAssetId> = scope
                    .intermediate_asset_ids
                    .iter()
                    .filter(|asset| market.units.contains(asset))
                    .cloned()
                    .collect();
                let outcome = find_best_conversion(
                    &index,
                    &ConversionRequest {
                        from_asset_id: start.clone(),
                        to_asset_id: target.clone(),
                        amount_in,
                        max_hops: 3,
                        max_paths: 32,
                        max_expansions: 4_000,
                        alternative_limit: 0,
                        allowed_intermediate_asset_ids: Some(routable),
                        fee_policy: FeePolicy::None,
                    },
                    &SearchCancellation::default(),
                );
                let verdict = match &outcome {
                    Ok(result) => match &result.best_path {
                        Some(path) => format!(
                            "out={} filled={}",
                            path.amount_out.quanta, path.is_fully_filled
                        ),
                        None => "NO-PATH".to_owned(),
                    },
                    Err(error) => format!("{error:?}"),
                };
                routed.push_str(&format!(" [{stake}: {verdict}]"));
            }
            println!(
                "{:<26} -> {:<26} unit={} routable_target={} direct[{direct_state}]{routed}",
                start.as_str(),
                target.as_str(),
                market.units.contains(target),
                scope.intermediate_asset_ids.contains(target),
            );
        }
    }

    // How the cycle walk actually grows with its length bound, on this book
    // rather than in the abstract. The captured graph is hub-and-spoke --
    // most currencies are only ever priced against the settlement pair -- so
    // the exponent that a complete market would carry is not the one here,
    // and the only way to know where the knee is is to walk it.
    {
        let index =
            MarketDepthIndex::try_from_selection(&market.instant_selection, market.units.clone())
                .map_err(|error| format!("index: {error:?}"))?;
        for length in 3..=7u8 {
            let started = std::time::Instant::now();
            let mut found = 0usize;
            let mut profitable = 0usize;
            for start in &policy.core_liquidity {
                let result = ptt_trade_engine::find_triangle_opportunities(
                    &index,
                    &ptt_trade_engine::TriangleRequest {
                        start_asset_id: start.clone(),
                        amount_in: None,
                        minimum_profit_basis_points: 100,
                        max_cycle_length: length,
                        max_results: 500,
                        max_evaluations: 1_000_000,
                        fee_policy: FeePolicy::None,
                    },
                    &SearchCancellation::default(),
                );
                match result {
                    Ok(result) => {
                        found += result.diagnostics.evaluated_cycle_count as usize;
                        profitable += result.opportunities.len();
                    }
                    Err(error) => {
                        println!("  length {length}: {error:?}");
                        break;
                    }
                }
            }
            println!(
                "cycle length <= {length}: {found} cycles walked, {profitable} profitable, {:.0}ms",
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    // What the page itself would print, so the diagnosis and the product
    // cannot disagree about the same database.
    println!("\n--- the Opportunities page, verbatim ---");
    let model = opportunities_model(
        observations,
        context_key,
        league,
        tuning,
        UiLanguage::English,
        None,
    )?;
    if let RadarScan::Ran(scan) = &model.scan {
        println!("diagnostics: {:?}", scan.diagnostics);
    }
    for line in opportunities_report(
        observations,
        context_key,
        league,
        tuning,
        UiLanguage::Chinese,
    )? {
        println!("{line}");
    }
    Ok(())
}

fn focus_coverage(
    observations: &[MarketEdgeObservation],
    context_key: &str,
    policy: &MarketPolicy,
    tuning: &MarketTuning,
    seen: &[MarketAssetId],
    // The caller's already-built Instant selection, when it has one. Coverage
    // needs three views of the book and one of them is the Instant view the
    // watchlist just computed; rebuilding it doubled the book construction on
    // the UI thread.
    prebuilt: Option<&Market>,
) -> Result<
    (
        ptt_workflows::FocusScopeStatus,
        Vec<ptt_workflows::FocusCoverage>,
        Vec<ptt_workflows::ProbeCandidate>,
    ),
    String,
> {
    let items = focus_items_from(policy, tuning, seen, FocusPurpose::Attention);
    let scope = FocusScope::try_new(&items, FocusScopePolicy::default())
        .map_err(|error| format!("{error}"))?;

    // Coverage needs three views of one book. The Instant view is the one the
    // caller already built, so it is borrowed rather than recomputed.
    let owned;
    let market = match prebuilt {
        Some(market) => market,
        None => {
            owned = build_market(observations, context_key, tuning, UiLanguage::English)?;
            &owned
        }
    };
    let now = Utc::now();
    let mut selections = Vec::new();
    for strategy in [
        QuoteSelectionStrategy::FastMaker,
        QuoteSelectionStrategy::Probe,
    ] {
        let (policy, _note) = selection_policy_from(tuning, strategy, UiLanguage::English)?;
        selections.push(
            select_quote_edges(&market.book, &policy, now)
                .map_err(|error| format!("select: {error}"))?,
        );
    }

    let (entries, candidates) = derive_focus_probe_candidates(
        "live-focus",
        &scope,
        &market.instant_selection,
        &selections[0],
        &selections[1],
    )
    .map_err(|error| format!("{error}"))?;
    Ok((scope.status, entries, candidates))
}

/// The same coverage, rendered for the watchlist page.
fn render_coverage(coverage: &CoverageModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::fill;
    let text = crate::report_text::report(language);
    let mut lines = Vec::new();
    if coverage.status == ptt_workflows::FocusScopeStatus::MissingTarget {
        lines.push(text.focus_has_no_targets.to_owned());
    }
    let incomplete: Vec<_> = coverage
        .entries
        .iter()
        .filter(|entry| entry.status != ptt_workflows::FocusCoverageStatus::Complete)
        .collect();
    lines.push(fill(
        text.coverage_progress,
        &[
            &(coverage.entries.len() - incomplete.len()).to_string(),
            &coverage.entries.len().to_string(),
        ],
    ));
    for entry in incomplete.iter().take(8) {
        lines.push(format!(
            "  {} -> {}  {}",
            entry.from_asset_id.as_str(),
            entry.to_asset_id.as_str(),
            crate::report_text::focus_coverage_status(language, entry.status),
        ));
    }
    for candidate in coverage.candidates.iter().take(8) {
        lines.push(format!(
            "  {} {}: {} -> {}  ({})",
            text.probe,
            crate::report_text::probe_priority(language, candidate.priority),
            candidate.from_asset_id.as_str(),
            candidate.to_asset_id.as_str(),
            crate::report_text::probe_reason(language, candidate.reason),
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// Analytics: season-scale value, supply/demand pressure, anchor health
// ---------------------------------------------------------------------------

/// Builds the analytics page model from persisted day rollups plus a live
/// fold of today's window observations. Pure over its inputs: the caller
/// fetches rollup rows, the window, and the season (see `rollup`).
#[must_use]
pub fn analytics_model(
    rollup_rows: &[ptt_storage::PairDayRollupRow],
    window_observations: &[MarketEdgeObservation],
    season: Option<&ptt_storage::SeasonRow>,
    league: &str,
    tuning: &MarketTuning,
    language: UiLanguage,
) -> AnalyticsModel {
    let text = crate::report_text::report(language);
    let mut notes = Vec::new();

    let (mut stats, row_notes) = crate::rollup::stats_from_rollup_rows(rollup_rows);
    notes.extend(row_notes);
    stats.extend(crate::rollup::today_stats(
        window_observations,
        Utc::now(),
        tuning.risk.top_book_outlier_factor,
    ));

    let (policy, policy_note) = market_policy_from(tuning, league, language);
    notes.extend(policy_note);

    let thresholds = match analytics_thresholds_from(tuning) {
        Some(thresholds) => thresholds,
        None => {
            notes.push(text.analytics_config_invalid.to_owned());
            ptt_strategy::AnalyticsThresholds::default()
        }
    };

    let data_days: std::collections::BTreeSet<&str> =
        stats.iter().map(|stat| stat.utc_day.as_str()).collect();
    let data_days = u32::try_from(data_days.len()).unwrap_or(u32::MAX);
    let pulse = ptt_strategy::market_pulse(&stats, &policy.core_liquidity, &thresholds);

    AnalyticsModel {
        notes,
        season: season.map(|row| SeasonBanner {
            label: row.label.clone(),
            started_day: row.started_at.format("%Y-%m-%d").to_string(),
        }),
        data_days,
        pulse,
    }
}

/// Raises a candidate one priority step when either side of its pair is a
/// scarce or high-turnover currency in the market pulse — the gaps that
/// cost the most to leave unprobed go to the front of the queue.
fn boost_probe_candidates(
    candidates: &mut [ptt_workflows::ProbeCandidate],
    pulse: Option<&ptt_strategy::MarketPulse>,
) {
    let Some(pulse) = pulse else {
        return;
    };
    let hot: std::collections::BTreeSet<&str> = pulse
        .assets
        .iter()
        .filter(|asset| asset.class == ptt_strategy::LiquidityClass::Scarce || asset.high_turnover)
        .map(|asset| asset.asset_id.as_str())
        .collect();
    for candidate in candidates.iter_mut() {
        if hot.contains(candidate.from_asset_id.as_str())
            || hot.contains(candidate.to_asset_id.as_str())
        {
            candidate.priority = candidate.priority.raised();
        }
    }
    // Ascending, because `ProbePriority` declares High first and derives `Ord`
    // from that order — High is the *smallest*. The sort is stable, so pairs
    // sharing a priority keep the alphabetical order dedup left them in.
    candidates.sort_by(|left, right| left.priority.cmp(&right.priority));
}

/// Risk thresholds from settings, plus each currency's own thin bar when the
/// market pulse has established a circulation norm: bar = norm × the
/// configured percent. A hundred mirrors is a deep book and a hundred chaos
/// is nothing — the norm says which, one constant cannot.
fn risk_thresholds_from(
    tuning: &MarketTuning,
    pulse: Option<&ptt_strategy::MarketPulse>,
) -> ptt_strategy::RiskThresholds {
    let mut thresholds = ptt_strategy::RiskThresholds {
        thin_liquidity_stock: tuning.risk.thin_liquidity_stock,
        asset_thin_thresholds: std::collections::BTreeMap::new(),
    };
    let Some(pulse) = pulse else {
        return thresholds;
    };
    let percent = u128::from(tuning.analytics.thin_norm_percent.min(100));
    if percent == 0 {
        return thresholds;
    }
    for asset in &pulse.assets {
        if let Some(norm) = asset.circulation_norm_units {
            // Floor division; a bar of zero would mark nothing thin, so it
            // falls back to the global constant instead.
            let bar = norm.saturating_mul(percent) / 100;
            if bar > 0 {
                thresholds.asset_thin_thresholds.insert(
                    asset.asset_id.as_str().to_owned(),
                    u64::try_from(bar).unwrap_or(u64::MAX),
                );
            }
        }
    }
    thresholds
}

/// Anomaly bars from settings — the same numbers every other risk site
/// reads, ending price_curve's private constants.
fn anomaly_thresholds_from(tuning: &MarketTuning) -> ptt_strategy::AnomalyThresholds {
    let bps = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    ptt_strategy::AnomalyThresholds {
        spike_basis_points: bps(tuning.risk.spike_basis_points),
        severe_spike_basis_points: bps(tuning.risk.severe_spike_basis_points),
        wide_spread_basis_points: bps(tuning.risk.wide_spread_basis_points),
        severe_spread_basis_points: bps(tuning.risk.severe_spread_basis_points),
        thin_stock: tuning.risk.thin_liquidity_stock,
    }
}

/// The tuned analytics thresholds, or `None` when the settings values do not
/// validate (the caller notes the degradation and uses the defaults).
fn analytics_thresholds_from(tuning: &MarketTuning) -> Option<ptt_strategy::AnalyticsThresholds> {
    let analytics = &tuning.analytics;
    ptt_strategy::AnalyticsThresholds::try_new(
        u32::try_from(analytics.trend_recent_days).ok()?,
        u32::try_from(analytics.trend_window_days).ok()?,
        u32::try_from(analytics.breadth_threshold_percent).ok()?,
        i64::try_from(analytics.verdict_threshold_bps).ok()?,
        u32::try_from(analytics.scarce_ratio_percent).ok()?,
        u128::from(analytics.quiet_floor_anchor_units),
    )
    .ok()
}

/// The analytics model as text lines — the probe's verification surface and
/// the parity reference for the page.
#[must_use]
pub fn analytics_report_lines(model: &AnalyticsModel, language: UiLanguage) -> Vec<String> {
    use crate::report_text::{
        anchor_drift, fill, liquidity_class, signed_percent_from_basis_points, trend_verdict,
    };
    let text = crate::report_text::report(language);
    let mut lines = model.notes.clone();
    if let Some(season) = &model.season {
        lines.push(fill(
            text.analytics_season_line,
            &[&season.label, &season.started_day],
        ));
    }
    let Some(as_of) = &model.pulse.as_of_day else {
        lines.push(text.analytics_no_data.to_owned());
        return lines;
    };
    lines.push(fill(
        text.analytics_as_of,
        &[as_of, &model.data_days.to_string()],
    ));
    if let Some(health) = &model.pulse.anchor_health {
        lines.push(fill(
            text.analytics_anchor_line,
            &[
                health.anchor_asset_id.as_str(),
                anchor_drift(language, health.drift),
            ],
        ));
        let median = health
            .market_median_move_bps
            .map_or_else(|| "-".to_owned(), signed_percent_from_basis_points);
        lines.push(fill(
            text.analytics_breadth_line,
            &[
                &health.risers.to_string(),
                &health.fallers.to_string(),
                &health.flat.to_string(),
                &median,
            ],
        ));
        for cross in &health.crosses {
            let drift = cross
                .drift_bps
                .map_or_else(|| "-".to_owned(), signed_percent_from_basis_points);
            lines.push(fill(
                text.analytics_cross_line,
                &[cross.asset_id.as_str(), &cross.latest_rate.text, &drift],
            ));
        }
    }
    lines.push(text.analytics_table_header.to_owned());
    for asset in &model.pulse.assets {
        let value = asset
            .value_in_anchor
            .as_ref()
            .map_or_else(|| "-".to_owned(), |rate| rate.text.clone());
        let supply = asset.supply_anchor.map_or_else(
            || format!("{}?", asset.supply_units),
            |units| units.to_string(),
        );
        let demand = asset.demand_anchor.map_or_else(
            || format!("{}?", asset.demand_units),
            |units| units.to_string(),
        );
        let verdict = asset
            .verdict
            .map_or("-", |verdict| trend_verdict(language, verdict));
        let mut markers = String::new();
        if asset.high_turnover {
            markers.push_str("  [");
            markers.push_str(text.analytics_marker_high_turnover);
            markers.push(']');
        }
        if asset.greedy_candidate {
            markers.push_str("  [");
            markers.push_str(text.analytics_marker_greedy);
            markers.push(']');
        }
        lines.push(format!(
            "{:<28} {:>12} {:>14} {:>14}  {}  {}{}",
            asset.asset_id.as_str(),
            value,
            supply,
            demand,
            liquidity_class(language, asset.class),
            verdict,
            markers,
        ));
    }
    lines
}

#[cfg(test)]
mod focus_scope_tests {
    use super::*;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn tuning(focus: &[&str], watch_only: &[&str]) -> MarketTuning {
        MarketTuning {
            focus_assets: focus.iter().map(|id| (*id).to_owned()).collect(),
            watch_only_assets: watch_only.iter().map(|id| (*id).to_owned()).collect(),
            ..MarketTuning::default()
        }
    }

    fn role_of(items: &[FocusGroupItem], id: &str) -> Option<FocusRole> {
        items
            .iter()
            .find(|item| item.asset_id.as_str() == id)
            .map(|item| item.role)
    }

    /// A currency the user has not listed is still worth arbitrage.
    ///
    /// Curating a focus list used to silence every other captured currency:
    /// the moment the feature was used as intended, opportunities the data
    /// already contained stopped being looked for. Being on the list buys
    /// priority and attention, not permission to be priced.
    #[test]
    fn an_unlisted_but_captured_currency_is_still_an_arbitrage_target() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&["omen-of-light"], &[]);
        let seen = vec![asset("omen-of-light"), asset("fracturing-orb")];

        let arbitrage = focus_items_from(&policy, &tuning, &seen, FocusPurpose::Arbitrage);
        assert_eq!(
            role_of(&arbitrage, "fracturing-orb"),
            Some(FocusRole::Target),
            "a captured currency is arithmetic, not a preference"
        );

        // The other half of the split: attention stays where the user put it.
        let attention = focus_items_from(&policy, &tuning, &seen, FocusPurpose::Attention);
        assert_eq!(role_of(&attention, "fracturing-orb"), None);
        assert_eq!(
            role_of(&attention, "omen-of-light"),
            Some(FocusRole::Target)
        );
    }

    /// The list decides the order, and the order decides the budget.
    #[test]
    fn listed_targets_come_before_captured_ones() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&["omen-of-light"], &[]);
        let seen = vec![asset("fracturing-orb"), asset("omen-of-light")];
        let items = focus_items_from(&policy, &tuning, &seen, FocusPurpose::Arbitrage);
        let targets: Vec<&str> = items
            .iter()
            .filter(|item| item.role == FocusRole::Target)
            .map(|item| item.asset_id.as_str())
            .collect();
        assert_eq!(targets.first(), Some(&"omen-of-light"));
    }

    /// Watch-only is the one exclusion a list still performs.
    #[test]
    fn watch_only_survives_being_captured() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&["omen-of-light"], &["fracturing-orb"]);
        let seen = vec![asset("fracturing-orb")];
        for purpose in [FocusPurpose::Arbitrage, FocusPurpose::Attention] {
            let items = focus_items_from(&policy, &tuning, &seen, purpose);
            assert_eq!(
                role_of(&items, "fracturing-orb"),
                Some(FocusRole::WatchOnly),
                "{purpose:?} must not promote a demoted currency"
            );
        }
    }

    /// With nothing configured, both answers are "everything seen" -- the
    /// behaviour before the split, unchanged.
    #[test]
    fn an_empty_list_scopes_over_everything_seen() {
        let policy = MarketPolicy::default_for("test-league");
        let tuning = tuning(&[], &[]);
        let seen = vec![asset("fracturing-orb")];
        for purpose in [FocusPurpose::Arbitrage, FocusPurpose::Attention] {
            let items = focus_items_from(&policy, &tuning, &seen, purpose);
            assert_eq!(role_of(&items, "fracturing-orb"), Some(FocusRole::Target));
        }
    }
}

#[cfg(test)]
mod radar_tests {
    use super::*;

    const CONTEXT: &str = "radar-test-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// The line a user reads when the radar succeeds must actually render.
    ///
    /// Built by hand rather than searched for: the captured corpus contains no
    /// arbitrage, so driving this branch through the search would need a
    /// synthetic market whose depth index cooperates, and that tests the
    /// engine rather than these four lines of formatting.
    #[test]
    fn a_found_opportunity_renders() {
        let path: Vec<MarketAssetId> = ["divine-orb", "chaos-orb", "exalted-orb"]
            .into_iter()
            .map(asset)
            .collect();
        let units = AssetUnitCatalog::try_new(
            path.iter()
                .map(|id| (id.clone(), AssetUnit::whole()))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("units");
        let item = ptt_workflows::RadarItem {
            item_id: "test-item".to_owned(),
            kind: ptt_workflows::RadarItemKind::BestConversion,
            category: ptt_strategy::Actionability::InstantExecutable,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 4000, &units)
                .expect("out"),
            value_basis_points: Some(30_012),
            liquidity_capacity: None,
            reasons: vec![ptt_workflows::RadarReason::BetterThanDirect],
            risk_flags: Vec::new(),
            blocking_risks: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let lines = radar_item_lines(
            &item,
            Some(RouteRate {
                numerator: 400,
                denominator: 1,
            }),
            None,
            UiLanguage::English,
        );
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("divine-orb -> chaos-orb -> exalted-orb"),
            "the route is not shown: {joined}"
        );
        assert!(
            joined.contains("300.12%"),
            "basis points are not rendered as a percentage: {joined}"
        );
        assert!(
            joined.contains("executable now"),
            "the execution category is missing: {joined}"
        );
        // The rate where the walked payout used to be: the scan's own walk
        // runs at a canonical size nobody holds, so its output said nothing
        // about the reader — the rate is what they act on.
        assert!(
            joined.contains("400 : 1"),
            "the route's front rate is missing: {joined}"
        );
        assert!(
            !joined.contains("out 4000"),
            "the canonical-size payout is not a number about the reader: {joined}"
        );
        assert!(
            joined.contains("better than direct"),
            "the reason is missing: {joined}"
        );

        // The same row for a Chinese reader. Nothing about a radar row is
        // language-specific except its words, so a row that renders in one
        // language and not the other means a value reached the screen as a
        // bare Rust identifier -- which is what this whole path replaced.
        let chinese = radar_item_lines(&item, None, None, UiLanguage::Chinese).join(
            "
",
        );
        assert!(
            chinese.contains("divine-orb -> chaos-orb -> exalted-orb"),
            "asset ids are the game's, not the interface's: {chinese}"
        );
        assert!(chinese.contains("300.12%"), "{chinese}");
        assert!(
            chinese.contains("现在就能成交"),
            "the execution category is still English: {chinese}"
        );
        assert!(
            chinese.contains("优于直兑"),
            "the reason is still English: {chinese}"
        );
        assert!(
            !chinese.contains("better than direct"),
            "both languages came out at once: {chinese}"
        );
    }

    /// An unpriced item must not print a bogus percentage.
    #[test]
    fn an_unpriced_item_says_unpriced() {
        let path: Vec<MarketAssetId> = ["divine-orb", "exalted-orb"]
            .into_iter()
            .map(asset)
            .collect();
        let units = AssetUnitCatalog::try_new(
            path.iter()
                .map(|id| (id.clone(), AssetUnit::whole()))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("units");
        let item = ptt_workflows::RadarItem {
            item_id: "unpriced".to_owned(),
            kind: ptt_workflows::RadarItemKind::Loop,
            category: ptt_strategy::Actionability::ProbeRequired,
            path_asset_ids: path.clone(),
            amount_in: AssetAmount::from_whole_units(asset("divine-orb"), 10, &units).expect("in"),
            amount_out: AssetAmount::from_whole_units(asset("exalted-orb"), 11, &units)
                .expect("out"),
            value_basis_points: None,
            liquidity_capacity: None,
            reasons: Vec::new(),
            risk_flags: Vec::new(),
            blocking_risks: Vec::new(),
            conversion_path: None,
            triangle: None,
        };
        let joined = radar_item_lines(&item, None, None, UiLanguage::English).join(
            "
",
        );
        assert!(joined.contains("unpriced"), "{joined}");
        assert!(joined.contains("capture more before trusting"), "{joined}");
        let chinese = radar_item_lines(&item, None, None, UiLanguage::Chinese).join(
            "
",
        );
        assert!(chinese.contains("数据不够，先多抓几次"), "{chinese}");
    }

    /// A book with nothing in it must say so rather than error.
    #[test]
    fn an_empty_book_says_so() {
        let lines = opportunities_report(
            &[],
            CONTEXT,
            "test-league",
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report");
        assert!(!lines.is_empty(), "the radar must always say something");
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    const CONTEXT: &str = "settlement-test-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// A taker row between any two assets, captured `age_seconds` ago.
    fn aged_taker(
        from: &str,
        to: &str,
        rate: (u64, u64),
        stock: u64,
        age_seconds: i64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::seconds(age_seconds);
        taker_at(from, to, rate, stock, captured)
    }

    /// A fresh taker row between any two assets.
    fn taker(from: &str, to: &str, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        taker_at(from, to, rate, stock, captured)
    }

    fn taker_at(
        from: &str,
        to: &str,
        rate: (u64, u64),
        stock: u64,
        captured: chrono::DateTime<Utc>,
    ) -> MarketEdgeObservation {
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: format!("{from}->{to}"),
                snapshot_id: format!("snapshot-{from}-{to}"),
                quote_id: format!("quote-{from}-{to}"),
                context_key: CONTEXT.to_owned(),
                from_asset_id: asset(from),
                to_asset_id: asset(to),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: QuoteSide::Available,
                execution_type: ExecutionType::Taker,
                role: QuoteEdgeRole::AvailableTaker,
                stock,
                original_need_asset_id: asset(to),
                original_have_asset_id: asset(from),
                original_row_index: 0,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    /// One physical arbitrage loop, visible from both settlement starts.
    /// divine -> chaos x100, chaos -> exalted x2, exalted -> divine /100:
    /// the cycle multiplies holdings by 2 from either entry, exactly, so
    /// both scans find it — and the report must show it once, not twice.
    #[test]
    fn the_same_cycle_from_two_settlement_starts_appears_once() {
        let observations = vec![
            taker("divine-orb", "chaos-orb", (100, 1), 10_000_000),
            taker("chaos-orb", "exalted-orb", (2, 1), 10_000_000),
            taker("exalted-orb", "divine-orb", (1, 100), 10_000_000),
        ];
        let tuning = MarketTuning::default();
        let lines = opportunities_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join(
            "
",
        );

        assert!(
            joined.contains("scanning from divine-orb, chaos-orb"),
            "both settlement currencies must be scanned from: {joined}"
        );
        // The kind column, not the word: reason lines also say "loop".
        let loop_lines = lines
            .iter()
            .filter(|line| line.contains("  loop  "))
            .count();
        assert_eq!(
            loop_lines, 1,
            "one physical loop must appear exactly once: {joined}"
        );
        assert!(
            joined.contains("100.00%"),
            "the loop doubles holdings — 10000bp: {joined}"
        );
    }

    /// The risk ladder the radar shows is the strategy ladder, thresholds
    /// included: raising the thin-liquidity bar above every stock in the
    /// book must flag every leg — with the typed reason, not a category
    /// downgrade alone.
    #[test]
    fn the_thin_liquidity_threshold_comes_from_settings() {
        let observations = vec![
            taker("divine-orb", "chaos-orb", (100, 1), 10_000_000),
            taker("chaos-orb", "exalted-orb", (2, 1), 10_000_000),
            taker("exalted-orb", "divine-orb", (1, 100), 10_000_000),
        ];
        let tuning = MarketTuning {
            risk: ptt_settings::RiskTuning {
                thin_liquidity_stock: 20_000_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = opportunities_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("thin liquidity"),
            "every stock sits below the configured bar: {joined}"
        );
        assert!(
            joined.contains("capture more before trusting"),
            "a blocked item carries the ladder's verdict: {joined}"
        );
    }

    /// The configured settlement list drives the core-liquidity set; a
    /// wholly invalid configuration falls back to the shipped default with a
    /// visible line, never silently.
    #[test]
    fn the_settlement_setting_drives_the_core_list() {
        let observations = vec![taker("divine-orb", "chaos-orb", (100, 1), 1_000)];
        let tuning = MarketTuning {
            settlement_assets: vec!["chaos-orb".to_owned()],
            ..Default::default()
        };
        let lines = watchlist_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("core liquidity: chaos-orb")),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("divine-orb, chaos-orb")),
            "the default list must be replaced, not appended to: {lines:?}"
        );

        let broken = MarketTuning {
            settlement_assets: vec!["NOT AN ID".to_owned()],
            ..Default::default()
        };
        let lines = watchlist_report(
            &observations,
            CONTEXT,
            "test-league",
            &broken,
            UiLanguage::English,
        )
        .expect("report");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("settlement currencies in settings are invalid")),
            "an unusable configuration must say so: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("core liquidity: divine-orb")),
            "and the shipped default must carry the page: {lines:?}"
        );
    }

    /// A focus asset the book has never priced becomes a probe suggestion
    /// on the radar page — the loop that sends the user into the game to
    /// close the gap — instead of an engine error or a silent omission.
    #[test]
    fn a_never_captured_focus_asset_becomes_a_probe_suggestion() {
        let observations = vec![taker("divine-orb", "chaos-orb", (100, 1), 1_000)];
        let tuning = MarketTuning {
            focus_assets: vec!["perfect-chaos-orb".to_owned()],
            ..Default::default()
        };
        let lines = opportunities_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("flip divine-orb -> perfect-chaos-orb"),
            "the missing pair must be a suggestion, not an error: {joined}"
        );
        assert!(
            joined.contains("no forward quote"),
            "with its typed reason: {joined}"
        );
    }

    /// A currency with real buy pressure that is absent from the focus list
    /// gets a promotion suggestion carrying the evidence; focus members and
    /// settlement currencies do not. (Rewritten with the P10 evidence rule:
    /// what earns the suggestion is demand, never capture frequency.)
    #[test]
    fn an_outsider_with_buy_pressure_is_suggested_for_focus() {
        let observations = vec![
            taker("divine-orb", "chaos-orb", (100, 1), 1_000),
            // The divine-for-vaal book prices vaal at 1/50 divine and its
            // available side pays out 1000 divine seeking 50000 vaal: buy
            // pressure 50000 vaal = 1000 divine. The vaal-supply book lists
            // 1000 vaal = 20 divine. Demand-heavy, so it earns the row.
            taker("vaal-orb", "divine-orb", (1, 50), 1_000),
            taker("chaos-orb", "vaal-orb", (1, 2), 1_000),
            taker("exalted-orb", "chaos-orb", (1, 3), 1_000),
        ];
        let tuning = MarketTuning {
            focus_assets: vec!["exalted-orb".to_owned()],
            ..Default::default()
        };
        let lines = watchlist_report(
            &observations,
            CONTEXT,
            "test-league",
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join(
            "
",
        );
        assert!(
            joined.contains("consider adding vaal-orb to focus - buy pressure 1000 vs listed 20"),
            "the anchor-valued evidence must earn and label the suggestion: {joined}"
        );
        assert!(
            !joined.contains("consider adding exalted-orb"),
            "a focus member needs no promotion: {joined}"
        );
        assert!(
            !joined.contains("consider adding divine-orb"),
            "settlement currencies need no promotion: {joined}"
        );
    }

    /// The traffic light against the default 2h/6h bands: one hour is
    /// green, three is yellow, seven is red — and exactly 7200 seconds is
    /// still green, pinning the classifier's inclusive boundary.
    #[test]
    fn the_pair_light_follows_the_configured_bands() {
        for (age, expected) in [
            (3_600_i64, "green - fresh"),
            (7_200, "green - fresh"),
            (3 * 3_600, "yellow - verify in game before acting"),
            (7 * 3_600, "red - stale, recapture"),
        ] {
            let observations = vec![aged_taker("divine-orb", "chaos-orb", (100, 1), 1_000, age)];
            let lines = history_report(
                &observations,
                CONTEXT,
                &asset("divine-orb"),
                &asset("chaos-orb"),
                &MarketTuning::default(),
                UiLanguage::English,
            )
            .expect("report");
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!("data freshness: {expected}"))),
                "age {age}s must read {expected}: {lines:?}"
            );
        }
    }

    /// Thresholds that do not validate (fresh >= usable) degrade to the
    /// shipped default with a visible line — the page still renders.
    #[test]
    fn invalid_freshness_thresholds_degrade_loudly() {
        let observations = vec![aged_taker("divine-orb", "chaos-orb", (100, 1), 1_000, 60)];
        let tuning = MarketTuning {
            freshness: ptt_settings::FreshnessTuning {
                fresh_seconds: 21_600,
                usable_seconds: 7_200,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = history_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            &tuning,
            UiLanguage::English,
        )
        .expect("report");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("freshness thresholds in settings are invalid")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("data freshness: green - fresh")),
            "the default bands must still classify: {lines:?}"
        );
    }
}

#[cfg(test)]
mod structural_tests {
    use super::*;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn pulse_asset(id: &str, class: ptt_strategy::LiquidityClass) -> ptt_strategy::AssetPulse {
        ptt_strategy::AssetPulse {
            asset_id: asset(id),
            value_in_anchor: None,
            value_is_composed: false,
            supply_units: 0,
            demand_units: 0,
            supply_anchor: None,
            demand_anchor: None,
            listing_rows: 0,
            days_observed: 1,
            circulation_norm_units: None,
            trend_bps_raw: None,
            trend_bps_relative: None,
            verdict: None,
            value_by_day: Vec::new(),
            supply_by_day: Vec::new(),
            class,
            high_turnover: false,
            greedy_candidate: class == ptt_strategy::LiquidityClass::Scarce,
        }
    }

    /// The annotation is context, never a verdict: legs keep path order, a
    /// repeated asset notes once, an unknown asset notes nothing, and no
    /// pulse means no notes — the page renders identically to before.
    #[test]
    fn structural_notes_annotate_without_reordering() {
        let pulse = ptt_strategy::MarketPulse {
            as_of_day: Some("2026-08-22".to_owned()),
            anchor_asset_id: Some(asset("divine-orb")),
            anchor_health: None,
            assets: vec![
                pulse_asset(
                    "scroll-of-wisdom",
                    ptt_strategy::LiquidityClass::Oversupplied,
                ),
                pulse_asset("mirror-of-kalandra", ptt_strategy::LiquidityClass::Scarce),
            ],
        };
        let path = [
            asset("divine-orb"),
            asset("scroll-of-wisdom"),
            asset("mirror-of-kalandra"),
            asset("divine-orb"),
        ];

        let notes = structural_notes_for(&path, Some(&pulse));
        assert_eq!(notes.len(), 2, "unknown assets and repeats add nothing");
        assert_eq!(notes[0].asset_id.as_str(), "scroll-of-wisdom");
        assert!(
            notes[0].structurally_illiquid(),
            "oversupplied is the junk shape"
        );
        assert_eq!(notes[1].asset_id.as_str(), "mirror-of-kalandra");
        assert!(!notes[1].structurally_illiquid(), "scarce is not illiquid");
        assert!(notes[1].greedy_candidate);

        assert!(
            structural_notes_for(&path, None).is_empty(),
            "no pulse, no notes, no page change"
        );
    }

    /// Each currency's thin bar is its own norm: 25% of a mirror's median
    /// daily supply is a different number from 25% of chaos's, and a
    /// currency without history keeps the global constant.
    #[test]
    fn thin_liquidity_follows_each_currency_s_own_norm() {
        let mut mirror = pulse_asset("mirror-of-kalandra", ptt_strategy::LiquidityClass::Scarce);
        mirror.circulation_norm_units = Some(8); // 25% -> bar of 2
        let mut chaos = pulse_asset("chaos-orb", ptt_strategy::LiquidityClass::Balanced);
        chaos.circulation_norm_units = Some(4_000); // 25% -> bar of 1000
        let pulse = ptt_strategy::MarketPulse {
            as_of_day: Some("2026-08-22".to_owned()),
            anchor_asset_id: Some(asset("divine-orb")),
            anchor_health: None,
            assets: vec![mirror, chaos],
        };

        let thresholds = risk_thresholds_from(&MarketTuning::default(), Some(&pulse));
        assert_eq!(thresholds.thin_threshold_for("mirror-of-kalandra"), 2);
        assert_eq!(thresholds.thin_threshold_for("chaos-orb"), 1_000);
        assert_eq!(
            thresholds.thin_threshold_for("never-seen-orb"),
            100,
            "no norm falls back to the global constant"
        );

        let without_pulse = risk_thresholds_from(&MarketTuning::default(), None);
        assert!(
            without_pulse.asset_thin_thresholds.is_empty(),
            "no history, no norms, existing behavior"
        );
    }

    /// A gap touching a scarce or high-turnover currency jumps the probe
    /// queue one step; everything else keeps its priority.
    #[test]
    fn scarce_and_high_turnover_gaps_probe_first() {
        let pulse = ptt_strategy::MarketPulse {
            as_of_day: Some("2026-08-22".to_owned()),
            anchor_asset_id: Some(asset("divine-orb")),
            anchor_health: None,
            assets: vec![pulse_asset(
                "mirror-of-kalandra",
                ptt_strategy::LiquidityClass::Scarce,
            )],
        };
        let candidate = |from: &str, to: &str| ptt_workflows::ProbeCandidate {
            from_asset_id: asset(from),
            to_asset_id: asset(to),
            reason: ptt_workflows::ProbeReason::MissingForwardQuote,
            source: ptt_workflows::ProbeSource::FocusGroup,
            priority: ptt_workflows::ProbePriority::Low,
            related_focus_group_id: None,
            last_seen_at: None,
            freshness_status: None,
            expected_value_hint: None,
            notes: None,
        };
        let mut candidates = vec![
            candidate("divine-orb", "mirror-of-kalandra"),
            candidate("divine-orb", "chaos-orb"),
        ];
        boost_probe_candidates(&mut candidates, Some(&pulse));
        assert_eq!(
            candidates[0].priority,
            ptt_workflows::ProbePriority::Medium,
            "the scarce pair moved up one step"
        );
        assert_eq!(
            candidates[1].priority,
            ptt_workflows::ProbePriority::Low,
            "unrelated pairs keep their place"
        );
    }

    /// Raising the label is not enough: both pages slice the head of the
    /// queue (four on Opportunities, eight on Watchlist), so a scarce gap
    /// that sorts late alphabetically stays invisible unless the raise also
    /// moves it.
    #[test]
    fn boosted_gaps_reach_the_displayed_head_of_the_queue() {
        let pulse = ptt_strategy::MarketPulse {
            as_of_day: Some("2026-08-22".to_owned()),
            anchor_asset_id: Some(asset("divine-orb")),
            anchor_health: None,
            assets: vec![pulse_asset(
                "mirror-of-kalandra",
                ptt_strategy::LiquidityClass::Scarce,
            )],
        };
        let candidate = |from: &str, to: &str| ptt_workflows::ProbeCandidate {
            from_asset_id: asset(from),
            to_asset_id: asset(to),
            reason: ptt_workflows::ProbeReason::MissingForwardQuote,
            source: ptt_workflows::ProbeSource::FocusGroup,
            priority: ptt_workflows::ProbePriority::Low,
            related_focus_group_id: None,
            last_seen_at: None,
            freshness_status: None,
            expected_value_hint: None,
            notes: None,
        };
        // Alphabetical, exactly as `deduplicate_probe_candidates` leaves them.
        // The scarce pair sorts last and starts well past the display slice.
        let mut candidates = vec![
            candidate("alteration-orb", "chaos-orb"),
            candidate("annulment-orb", "chaos-orb"),
            candidate("blessed-orb", "chaos-orb"),
            candidate("chance-orb", "chaos-orb"),
            candidate("chaos-orb", "divine-orb"),
            candidate("regal-orb", "mirror-of-kalandra"),
        ];
        boost_probe_candidates(&mut candidates, Some(&pulse));

        let position = candidates
            .iter()
            .position(|entry| entry.to_asset_id.as_str() == "mirror-of-kalandra")
            .expect("the scarce pair is still in the queue");
        let priority = candidates[position].priority;
        assert_eq!(
            priority,
            ptt_workflows::ProbePriority::Medium,
            "the scarce pair was raised one step"
        );
        assert!(
            position < 4,
            "the scarce pair is {priority:?} but sits at index {position}, past the four \
             candidates Opportunities shows — the raise never reordered the queue"
        );
    }

    /// Monitor reads the queue through `probe_queue_model`, which had no pulse
    /// at all: the gaps that jumped the queue on Watchlist sat unmoved on the
    /// page that is on screen permanently. A pulse has to raise and reorder
    /// here too, raise nothing that does not touch the scarce currency, and
    /// no pulse has to leave the queue exactly as it was.
    #[test]
    fn the_monitor_queue_boosts_only_when_it_is_given_a_pulse() {
        use ptt_trade_domain::{
            Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio,
            SnapshotRecordStatus,
        };

        const CONTEXT: &str = "monitor-queue-test";

        let edge = |need: &str, have: &str, stock: u64| {
            let captured = Utc::now() - chrono::Duration::minutes(1);
            MarketEdgeObservation {
                edge: QuoteEdge {
                    edge_id: format!("{need}-{have}-{stock}"),
                    snapshot_id: format!("snapshot-{need}-{have}"),
                    quote_id: format!("quote-{need}-{have}"),
                    context_key: CONTEXT.to_owned(),
                    from_asset_id: asset(have),
                    to_asset_id: asset(need),
                    rate: Ratio::from_parts(1, 5).expect("rate"),
                    source_side: QuoteSide::Available,
                    execution_type: ExecutionType::Taker,
                    role: QuoteEdgeRole::AvailableTaker,
                    stock,
                    original_need_asset_id: asset(need),
                    original_have_asset_id: asset(have),
                    original_row_index: 0,
                    comparator: Comparator::Exact,
                    user_edited: false,
                    machine_confidence_ppm: Some(990_000),
                    captured_at: captured,
                    confirmed_at: captured,
                },
                snapshot_complete: true,
                record_status: SnapshotRecordStatus::Active,
                record_revision: 1,
                record_reason: None,
            }
        };

        // Taker side only, so every pair is missing something and the queue
        // has candidates to order.
        let observations = vec![
            edge("chaos-orb", "divine-orb", 100),
            edge("exalted-orb", "divine-orb", 50),
            edge("mirror-of-kalandra", "divine-orb", 1),
        ];
        let pulse = ptt_strategy::MarketPulse {
            as_of_day: Some("2026-08-22".to_owned()),
            anchor_asset_id: Some(asset("divine-orb")),
            anchor_health: None,
            assets: vec![pulse_asset(
                "mirror-of-kalandra",
                ptt_strategy::LiquidityClass::Scarce,
            )],
        };
        let tuning = MarketTuning::default();

        let queue = |market_pulse: Option<&ptt_strategy::MarketPulse>| {
            let model = probe_queue_model(
                &observations,
                CONTEXT,
                "test-league",
                &tuning,
                UiLanguage::English,
                market_pulse,
            )
            .expect("probe queue");
            model
                .candidates
                .iter()
                .map(|entry| {
                    (
                        (
                            entry.from_asset_id.as_str().to_owned(),
                            entry.to_asset_id.as_str().to_owned(),
                            entry.reason,
                        ),
                        entry.priority,
                    )
                })
                .collect::<Vec<_>>()
        };

        let before: std::collections::BTreeMap<_, _> = queue(None).into_iter().collect();
        let after = queue(Some(&pulse));
        assert!(
            !after.is_empty(),
            "the fixture has to produce candidates for this test to mean anything"
        );

        let mut raised = Vec::new();
        for (key, priority) in &after {
            let was = before
                .get(key)
                .copied()
                .expect("the boost must not invent candidates");
            if *priority != was {
                assert!(
                    *priority < was,
                    "the boost may only raise: {key:?} went {was:?} -> {priority:?}"
                );
                raised.push(key.clone());
            }
        }
        assert!(
            !raised.is_empty(),
            "the pulse never reached the boost — Monitor is still unwired"
        );
        assert!(
            raised.iter().all(|(from, to, _)| {
                from.as_str() == "mirror-of-kalandra" || to.as_str() == "mirror-of-kalandra"
            }),
            "only gaps touching the scarce currency may be raised: {raised:?}"
        );
        assert!(
            after.windows(2).all(|pair| pair[0].1 <= pair[1].1),
            "the queue Monitor slices is not in priority order: {after:?}"
        );
    }
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// One panel-orientation edge of a (need, have) book.
    fn book_edge(
        snapshot: &str,
        need: &str,
        have: &str,
        side: QuoteSide,
        rate: (u64, u64),
        stock: u64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        let (role, execution) = match side {
            QuoteSide::Available => (QuoteEdgeRole::AvailableTaker, ExecutionType::Taker),
            QuoteSide::Competing => (
                QuoteEdgeRole::CompetingMakerReference,
                ExecutionType::MakerReference,
            ),
        };
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: format!("{snapshot}-{need}-{have}-{side:?}-{stock}"),
                snapshot_id: snapshot.to_owned(),
                quote_id: format!("quote-{snapshot}-{stock}"),
                context_key: "suggestion-test-context".to_owned(),
                from_asset_id: asset(have),
                to_asset_id: asset(need),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: side,
                execution_type: execution,
                role,
                stock,
                original_need_asset_id: asset(need),
                original_have_asset_id: asset(have),
                original_row_index: 0,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    /// The user-adjudicated evidence rule: capture frequency proves only
    /// attention. An asset captured five times with nothing on its demand
    /// side is never suggested; an asset seen once with real buy pressure is.
    #[test]
    fn suggestions_follow_buy_pressure_not_capture_frequency() {
        let mut observations = Vec::new();
        // Flipped five times, supply only: nobody is buying it.
        for snapshot in 0..5 {
            observations.push(book_edge(
                &format!("shiny-{snapshot}"),
                "shiny-bauble",
                "divine-orb",
                QuoteSide::Available,
                (10, 1),
                500,
            ));
        }
        // Seen once, with 400 divine of standing buy pressure: the book
        // (need=wanted, have=divine) prices it at 5 divine each and its
        // competing side holds 400 divine seeking it.
        observations.push(book_edge(
            "wanted-1",
            "wanted-orb",
            "divine-orb",
            QuoteSide::Available,
            (1, 5),
            20,
        ));
        observations.push(book_edge(
            "wanted-1",
            "wanted-orb",
            "divine-orb",
            QuoteSide::Competing,
            (1, 5),
            400,
        ));

        let tuning = MarketTuning::default();
        let (policy, _) = market_policy_from(&tuning, "test-league", UiLanguage::English);
        let suggestions = focus_suggestions(&observations, &policy, &tuning);

        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.asset_id.as_str() == "wanted-orb"),
            "real buy pressure must be suggested: {suggestions:?}"
        );
        assert!(
            suggestions
                .iter()
                .all(|suggestion| suggestion.asset_id.as_str() != "shiny-bauble"),
            "capture frequency alone must never be evidence: {suggestions:?}"
        );
        let wanted = suggestions
            .iter()
            .find(|suggestion| suggestion.asset_id.as_str() == "wanted-orb")
            .expect("wanted-orb");
        assert_eq!(
            wanted.demand_anchor, 400,
            "400 divine paid out by the competing side, anchor-valued"
        );

        // A dismissal at this prominence holds until the pressure doubles.
        let mut dismissed = tuning.clone();
        dismissed
            .ignored_suggestions
            .push(ptt_settings::IgnoredSuggestion {
                asset_id: "wanted-orb".to_owned(),
                snapshots_when_ignored: wanted.demand_anchor,
            });
        let after = focus_suggestions(&observations, &policy, &dismissed);
        assert!(
            after
                .iter()
                .all(|suggestion| suggestion.asset_id.as_str() != "wanted-orb"),
            "a dismissed suggestion stays down until it doubles: {after:?}"
        );
    }
}

#[cfg(test)]
mod convert_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    const CONTEXT: &str = "convert-test-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn observation(
        edge_id: &str,
        side: QuoteSide,
        role: QuoteEdgeRole,
        execution: ExecutionType,
        row: u8,
        rate: (u64, u64),
        stock: u64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: edge_id.to_owned(),
                snapshot_id: "snapshot-1".to_owned(),
                quote_id: format!("quote-{edge_id}"),
                context_key: CONTEXT.to_owned(),
                from_asset_id: asset("divine-orb"),
                to_asset_id: asset("chaos-orb"),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: side,
                execution_type: execution,
                role,
                stock,
                original_need_asset_id: asset("chaos-orb"),
                original_have_asset_id: asset("divine-orb"),
                original_row_index: row,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    fn taker(edge_id: &str, row: u8, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        observation(
            edge_id,
            QuoteSide::Available,
            QuoteEdgeRole::AvailableTaker,
            ExecutionType::Taker,
            row,
            rate,
            stock,
        )
    }

    fn competing(edge_id: &str, row: u8, rate: (u64, u64), stock: u64) -> MarketEdgeObservation {
        observation(
            edge_id,
            QuoteSide::Competing,
            QuoteEdgeRole::CompetingMakerReference,
            ExecutionType::MakerReference,
            row,
            rate,
            stock,
        )
    }

    /// "I hold 100" prices one size — the holding — and the ladder stays
    /// home. The maker section follows the same size.
    #[test]
    fn a_stated_holding_prices_exactly_that_size() {
        let observations = vec![
            taker("take-700", 0, (700, 1), 100_000),
            competing("front", 1, (784, 1), 40),
        ];
        let lines = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            Some(100),
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join("\n");
        assert!(
            joined.contains(" 100 divine-orb"),
            "the stated holding must be priced: {joined}"
        );
        assert!(
            !joined.contains("  10 divine-orb   via"),
            "the default ladder must not run alongside a stated holding: {joined}"
        );
        assert!(
            joined.contains("at size 100"),
            "the maker section follows the holding: {joined}"
        );
    }

    /// The Convert page's maker section, end to end from raw observations:
    /// the bait listing on the competing side is flagged by the book's
    /// median band, excluded from the queue math, and rendered with its
    /// reason — while the undercut prices against the honest front.
    #[test]
    fn the_maker_section_excludes_the_bait_and_prices_the_honest_front() {
        let observations = vec![
            taker("take-700", 0, (700, 1), 100_000),
            competing("bait", 0, (100, 1), 500),
            competing("front", 1, (784, 1), 40),
            competing("second", 2, (785, 1), 60),
            competing("back", 3, (795, 1), 80),
        ];
        let lines = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            None,
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join("\n");

        assert!(
            joined.contains("listing strategy divine-orb -> chaos-orb"),
            "the maker section is missing: {joined}"
        );
        assert!(
            joined.contains("take now at 700:1"),
            "the instant reference is missing: {joined}"
        );
        assert!(
            joined.contains("undercut, list at 783:1"),
            "the undercut must price one tick below the honest front: {joined}"
        );
        assert!(
            joined.contains("excluded listing at 100:1 (stock 500): price outlier"),
            "the bait must be visible with its reason: {joined}"
        );
        assert!(
            !joined.contains("list at 99:1"),
            "nothing may price against the bait: {joined}"
        );

        // The same section for a Chinese reader, and the exclusion reason
        // with it.
        let chinese = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            None,
            &MarketTuning::default(),
            UiLanguage::Chinese,
        )
        .expect("report")
        .join("\n");
        assert!(chinese.contains("挂单策略"), "{chinese}");
        assert!(chinese.contains("价格离群"), "{chinese}");
    }

    /// **A hazard that holds however you price inside a book is a property of
    /// the book, so the panel says it once.**
    ///
    /// The three modes differ only in *where* in the queue they list. The
    /// risks they were each printing — the quote is an aggregate row, the
    /// book is one listing deep, the reference is a maker quote — are true of
    /// the pair whichever way you price, so the panel repeated the same
    /// sentence three times and buried the one thing a mode row can say for
    /// itself.
    ///
    /// This book is deliberately plain: three competing rows too close
    /// together to form a wall, an instant every listing beats, and a front
    /// that undercuts cleanly. So no mode earns a risk of its own and the
    /// hoisted line carries all of it.
    #[test]
    fn the_pairs_risks_are_said_once_not_once_per_mode() {
        let observations = vec![
            taker("take-700", 0, (700, 1), 100_000),
            competing("front", 1, (784, 1), 40),
            competing("second", 2, (785, 1), 60),
            competing("back", 3, (795, 1), 80),
        ];
        let lines = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            None,
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report");
        let joined = lines.join("\n");

        assert_eq!(
            lines
                .iter()
                .filter(|line| line.trim_start().starts_with("risks"))
                .count(),
            1,
            "the pair's risks belong to the pair, so the panel says them once, \
             not once per mode:\n{joined}"
        );
        assert!(
            joined.contains("risks on this pair"),
            "and the surviving line says whose risks they are:\n{joined}"
        );

        let chinese = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            None,
            &MarketTuning::default(),
            UiLanguage::Chinese,
        )
        .expect("report")
        .join("\n");
        assert_eq!(
            chinese.matches("风险").count(),
            1,
            "the hoisted line goes through the bilingual catalogue too:\n{chinese}"
        );
    }
}

#[cfg(test)]
mod analytics_sign_tests {
    use super::*;
    use ptt_trade_domain::Ratio;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    fn model(median_bps: i64, cross_bps: i64) -> AnalyticsModel {
        AnalyticsModel {
            notes: Notes::new(),
            season: None,
            data_days: 5,
            pulse: ptt_strategy::MarketPulse {
                as_of_day: Some("2026-08-23".to_owned()),
                anchor_asset_id: Some(asset("chaos-orb")),
                anchor_health: Some(ptt_strategy::AnchorHealth {
                    anchor_asset_id: asset("chaos-orb"),
                    drift: ptt_strategy::AnchorDrift::Steady,
                    market_median_move_bps: Some(median_bps),
                    risers: 3,
                    fallers: 7,
                    flat: 4,
                    crosses: vec![ptt_strategy::AnchorCross {
                        asset_id: asset("divine-orb"),
                        latest_rate: Ratio::from_parts(111, 10).expect("rate"),
                        drift_bps: Some(cross_bps),
                    }],
                }),
                assets: Vec::new(),
            },
        }
    }

    /// A rise and a fall have to be told apart at a glance.
    ///
    /// The Analytics page writes a leading `+` on these two numbers because
    /// they are moves, not levels; the text lines are documented as the
    /// page's parity reference, so a positive move that reads `2.57%` in one
    /// place and `+2.57%` in the other makes the reader wonder whether they
    /// are even the same number.
    #[test]
    fn a_positive_analytics_drift_carries_its_plus_sign() {
        let lines = analytics_report_lines(&model(257, 724), UiLanguage::English);
        assert!(
            lines.iter().any(|line| line.contains("+2.57%")),
            "market median: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("+7.24%")),
            "anchor cross: {lines:?}"
        );
    }

    /// The minus is the shared formatter's, and adding a plus must not
    /// double it up.
    #[test]
    fn a_negative_analytics_drift_keeps_exactly_one_minus_sign() {
        let lines = analytics_report_lines(&model(-257, -724), UiLanguage::English);
        assert!(
            lines.iter().any(|line| line.contains("-2.57%")),
            "market median: {lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("+-") || line.contains("--")),
            "a sign was written twice: {lines:?}"
        );
    }
}

#[cfg(test)]
mod leg_coverage_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    const CONTEXT: &str = "leg-liquidity-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// One quote edge. `tag` names the capture: every edge a single panel
    /// produces has to share one snapshot id, because the coherent book keeps
    /// the newest snapshot **per panel side** and would otherwise throw away
    /// half of a fixture that pretended each direction was its own capture.
    #[allow(clippy::too_many_arguments)]
    fn edge(
        tag: &str,
        edge_id: &str,
        from: &str,
        to: &str,
        side: QuoteSide,
        role: QuoteEdgeRole,
        execution: ExecutionType,
        row: u8,
        rate: (u64, u64),
        stock: u64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: edge_id.to_owned(),
                snapshot_id: format!("snapshot-{tag}"),
                quote_id: format!("quote-{edge_id}"),
                context_key: CONTEXT.to_owned(),
                from_asset_id: asset(from),
                to_asset_id: asset(to),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: side,
                execution_type: execution,
                role,
                stock,
                original_need_asset_id: asset(to),
                original_have_asset_id: asset(from),
                original_row_index: row,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    /// One captured panel (need = `need`, have = `have`) as the two edges per
    /// row the domain builds: a taker one way and a maker reference back.
    ///
    /// The `available` rows are priced `to`-per-`from`, which is the direction
    /// the route takes in, and their stock counts what the lister pays out —
    /// the asset being bought. That is the whole point of this fixture: the
    /// competing rows carry a stock in the *other* currency, so what a leg can
    /// take cannot be read off the pair as a whole.
    fn panel(
        tag: &str,
        have: &str,
        need: &str,
        available: &[((u64, u64), u64)],
        competing: &[((u64, u64), u64)],
    ) -> Vec<MarketEdgeObservation> {
        let mut out = Vec::new();
        for (index, (rate, stock)) in available.iter().enumerate() {
            let row = u8::try_from(index).expect("row index");
            out.push(edge(
                tag,
                &format!("{tag}-a{index}-f"),
                have,
                need,
                QuoteSide::Available,
                QuoteEdgeRole::AvailableTaker,
                ExecutionType::Taker,
                row,
                *rate,
                *stock,
            ));
            out.push(edge(
                tag,
                &format!("{tag}-a{index}-r"),
                need,
                have,
                QuoteSide::Available,
                QuoteEdgeRole::AvailableReverseMakerReference,
                ExecutionType::MakerReference,
                row,
                (rate.1, rate.0),
                *stock,
            ));
        }
        for (index, (rate, stock)) in competing.iter().enumerate() {
            let row = u8::try_from(index).expect("row index");
            out.push(edge(
                tag,
                &format!("{tag}-c{index}-f"),
                have,
                need,
                QuoteSide::Competing,
                QuoteEdgeRole::CompetingMakerReference,
                ExecutionType::MakerReference,
                row,
                *rate,
                *stock,
            ));
            out.push(edge(
                tag,
                &format!("{tag}-c{index}-r"),
                need,
                have,
                QuoteSide::Competing,
                QuoteEdgeRole::CompetingReverseTaker,
                ExecutionType::Taker,
                row,
                (rate.1, rate.0),
                *stock,
            ));
        }
        out
    }

    /// The omen book behind the chaos leg: three rows, 120 omens in all, and
    /// only 12 of them at the front price. Shaped after the real 2026-08-23
    /// reading this whole signal came from.
    fn chaos_to_omen() -> Vec<MarketEdgeObservation> {
        panel(
            "co",
            "chaos-orb",
            "omen-orb",
            &[((1, 57), 12), ((1, 60), 40), ((1, 65), 68)],
            &[],
        )
    }

    fn legs_of(
        observations: &[MarketEdgeObservation],
        have: &str,
        need: &str,
        holdings: u64,
    ) -> Vec<LegTakeCoverage> {
        let model = convert_model(
            observations,
            CONTEXT,
            &asset(have),
            &asset(need),
            Some(holdings),
            &MarketTuning::default(),
            UiLanguage::English,
            None,
        )
        .expect("convert model");
        model
            .sizes
            .first()
            .expect("one priced size")
            .quotes
            .first()
            .expect("one visible route")
            .legs
            .clone()
    }

    /// The two bands, on one real book. 120 omens are listed against chaos;
    /// 35 of them is a large share and still just a trade, 175 is more than
    /// the listings hold.
    ///
    /// The amounts are the ask at the **front** rate (1:57), not the blended
    /// average of walking 1:57, 1:60 and 1:65. That is the whole basis of
    /// this page: the reader lists one rate and the market fills them at it
    /// or better, so "what would this step have to buy" is the ask through
    /// the front rows. The blended walk answers a different question and
    /// used to answer it in this column, which is why the header and the
    /// steps quoted different trips.
    #[test]
    fn a_leg_is_graded_by_how_much_of_the_listings_the_trip_takes() {
        let observations = chaos_to_omen();

        let covered = legs_of(&observations, "chaos-orb", "omen-orb", 100);
        assert_eq!(covered.len(), 1, "{covered:?}");
        assert_eq!(covered[0].listed, Some(120), "{covered:?}");
        assert_eq!(covered[0].taking, 1, "{covered:?}");
        assert_eq!(covered[0].verdict, LegTakeVerdict::Covered, "{covered:?}");

        let big_share = legs_of(&observations, "chaos-orb", "omen-orb", 2_000);
        assert_eq!(big_share[0].taking, 35, "{big_share:?}");
        assert_eq!(big_share[0].share_percent, Some(29), "{big_share:?}");
        assert_eq!(
            big_share[0].verdict,
            LegTakeVerdict::Covered,
            "29% of a book that holds it is a trade, not a hazard: {big_share:?}"
        );

        let short = legs_of(&observations, "chaos-orb", "omen-orb", 10_000);
        assert_eq!(short[0].taking, 175, "{short:?}");
        assert_eq!(
            short[0].verdict,
            LegTakeVerdict::NotEnoughListed,
            "past the whole book is not covered at any price: {short:?}"
        );
    }

    /// A middle currency is spent again immediately. The divine leg into
    /// chaos is nothing next to its own book, but that chaos then has to take
    /// omens through a 120-omen door, so the first leg is graded by the door.
    #[test]
    fn a_middle_leg_is_graded_by_whichever_book_is_tighter() {
        let mut observations = panel("dc", "divine-orb", "chaos-orb", &[((700, 1), 100_000)], &[]);
        observations.extend(chaos_to_omen());

        let legs = legs_of(&observations, "divine-orb", "omen-orb", 20);
        assert_eq!(legs.len(), 2, "{legs:?}");
        assert_eq!(
            legs[0].share_percent,
            Some(14),
            "the leg still reports its own number: {legs:?}"
        );
        assert_eq!(
            legs[0].verdict,
            LegTakeVerdict::NotEnoughListed,
            "the next book is the binding one: {legs:?}"
        );
        assert!(legs[0].bound_by_next_leg, "{legs:?}");
        assert_eq!(legs[1].verdict, LegTakeVerdict::NotEnoughListed, "{legs:?}");
        assert!(
            !legs[1].bound_by_next_leg,
            "the last leg has no next leg to be bound by: {legs:?}"
        );
    }

    /// **Every step is measured against the reader's ask, not against what
    /// the step before it managed to buy.**
    ///
    /// The engine hands the next leg the previous leg's *actual* output
    /// (route.rs propagates `net_amount_out`), so once any leg runs short
    /// every leg behind it inherits a shrunken request and reports a trip
    /// smaller than the reader asked for. On the owner's real book the last
    /// leg of a three-hop route read the same 27 at a 500 ask and at a
    /// 50,000 ask -- a warning that cannot move with the ask is not a
    /// warning. The route header is already priced at the front rates
    /// (`project_at_front_rates`), so the steps have to be too, or the two
    /// halves of one card are quoting different trips.
    #[test]
    fn every_step_is_measured_against_the_ask_not_against_the_step_before_it() {
        let mut observations = panel("db", "divine-orb", "bridge-orb", &[((1, 1), 5)], &[]);
        observations.extend(panel(
            "bc",
            "bridge-orb",
            "chaos-orb",
            &[((10, 1), 1_000_000)],
            &[],
        ));

        let small = legs_of(&observations, "divine-orb", "chaos-orb", 10);
        let large = legs_of(&observations, "divine-orb", "chaos-orb", 1_000);
        assert_eq!(small.len(), 2, "{small:?}");

        assert_eq!(small[0].taking, 10, "first leg already scales: {small:?}");
        assert_eq!(large[0].taking, 1_000, "{large:?}");
        assert_eq!(
            small[1].taking, 100,
            "ten divine at one bridge each at ten chaos each is a hundred chaos: {small:?}"
        );
        assert_eq!(
            large[1].taking, 10_000,
            "the second leg has to move with the ask too: {large:?}"
        );
    }

    /// Under one percent the share is left off for the same reason it is
    /// left off above the whole book: it says nothing the two amounts do not
    /// already say, and floored to "0%" beside a five-figure take it reads
    /// as a broken number rather than a small one. The owner's card showed
    /// "市面挂着 566708, 这一趟要吃掉 5275（0%）" -- 5,275 is not zero
    /// percent of anything, it is 0.93%.
    #[test]
    fn a_share_under_one_percent_is_not_printed_as_zero() {
        let observations = panel("big", "chaos-orb", "omen-orb", &[((1, 1), 1_000_000)], &[]);
        let legs = legs_of(&observations, "chaos-orb", "omen-orb", 5_275);
        assert_eq!(legs[0].listed, Some(1_000_000), "{legs:?}");
        assert_eq!(legs[0].taking, 5_275, "{legs:?}");
        assert_eq!(
            legs[0].share_percent, None,
            "under one percent says nothing the amounts do not: {legs:?}"
        );
        let facts = crate::report_text::leg_take_facts(UiLanguage::Chinese, "a", "b", &legs[0]);
        assert!(!facts.contains("0%"), "{facts}");
        assert!(facts.contains("5275"), "{facts}");
    }

    /// Past everything listed, the share is the verdict said again as noise:
    /// "takes 159 (132%)" repeats "more than everything listed", and at a
    /// large ask the repeat inflates into numbers like 1796364% that bury
    /// the two figures that matter. The amounts and the verdict carry it.
    /// Within the listings the share still prints — there it says something
    /// the verdict does not.
    #[test]
    fn a_share_past_everything_listed_is_not_printed() {
        let mut leg = LegTakeCoverage {
            from_asset_id: asset("divine-orb"),
            to_asset_id: asset("perfect-chaos-orb"),
            taking: 83_333_333,
            listed: Some(46),
            share_percent: Some(181_159_419),
            verdict: LegTakeVerdict::NotEnoughListed,
            bound_by_next_leg: false,
            single_listing: false,
        };
        for language in [UiLanguage::English, UiLanguage::Chinese] {
            let facts = crate::report_text::leg_take_facts(language, "divine", "perfect", &leg);
            assert!(
                !facts.contains('%'),
                "past the whole book the share is noise: {facts}"
            );
            assert!(facts.contains("83333333"), "{facts}");
            assert!(facts.contains("46"), "{facts}");
        }

        leg.taking = 15;
        leg.listed = Some(41);
        leg.share_percent = Some(36);
        leg.verdict = LegTakeVerdict::Covered;
        for language in [UiLanguage::English, UiLanguage::Chinese] {
            let facts = crate::report_text::leg_take_facts(language, "divine", "perfect", &leg);
            assert!(facts.contains("36%"), "within the book it informs: {facts}");
        }
    }

    /// A direction nobody captured is an absence, not a shortage. This
    /// project never infers the second from the first, so an empty book has
    /// to reach a different verdict from an overdrawn one.
    ///
    /// Stated on the classifier rather than through a page, because a leg
    /// with no listings has no route through it either — the search would
    /// never hand the page such a leg. The branch is a guarantee about what
    /// the page would say, not a state it can currently reach.
    #[test]
    fn an_uncaptured_direction_reads_as_no_data_rather_than_short() {
        assert_eq!(leg_take_verdict(500, 0), LegTakeVerdict::NoListings);
        assert_eq!(leg_take_verdict(500, 400), LegTakeVerdict::NotEnoughListed);
    }

    /// The text report is the page's parity reference, so the leg has to
    /// reach it too — with the numbers, in both languages, and worded as
    /// taking rather than waiting.
    #[test]
    fn the_text_report_prints_each_leg_against_its_own_listings() {
        let observations = chaos_to_omen();
        let english = convert_report(
            &observations,
            CONTEXT,
            &asset("chaos-orb"),
            &asset("omen-orb"),
            Some(10_000),
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report")
        .join(
            "
",
        );
        assert!(
            english.contains("chaos-orb -> omen-orb   120 listed, this trip takes 175"),
            "{english}"
        );
        assert!(english.contains("more than everything listed"), "{english}");

        let chinese = convert_report(
            &observations,
            CONTEXT,
            &asset("chaos-orb"),
            &asset("omen-orb"),
            Some(10_000),
            &MarketTuning::default(),
            UiLanguage::Chinese,
        )
        .expect("report")
        .join(
            "
",
        );
        assert!(
            chinese.contains("市面挂着 120，这一趟要吃掉 175"),
            "{chinese}"
        );
        assert!(chinese.contains("一次吃不完"), "{chinese}");
        assert!(
            !chinese.contains("卡住") && !chinese.contains("等"),
            "the line must not read as a maker question: {chinese}"
        );
    }

    /// Forty-one mirrors exist and taking fifteen is 36% of that, over the
    /// bar — but thirty sit at the front price, so the fill never leaves the
    /// quoted rate and there is nothing to warn about. The percentage is
    /// still printed; only the colour stands down.
    #[test]
    fn a_trip_the_front_row_alone_can_fill_is_not_flagged() {
        let observations = panel(
            "dm",
            "divine-orb",
            "mirror-orb",
            &[((1, 776), 30), ((1, 800), 11)],
            &[],
        );
        let legs = legs_of(&observations, "divine-orb", "mirror-orb", 11_640);
        assert_eq!(legs[0].listed, Some(41), "{legs:?}");
        assert_eq!(legs[0].taking, 15, "{legs:?}");
        assert_eq!(
            legs[0].share_percent,
            Some(36),
            "the number is shown either way: {legs:?}"
        );
        assert_eq!(
            legs[0].verdict,
            LegTakeVerdict::Covered,
            "the front row alone covers it: {legs:?}"
        );
    }
}

#[cfg(test)]
mod versus_direct_wording_tests {
    use super::*;
    use crate::report_text::versus_direct;
    use ptt_trade_engine::ComparisonDirection;

    /// The 2026-08-23 field reading: a route 1,338 basis points behind the
    /// direct trade.
    ///
    /// Both templates already carry the direction -- `比直兑低`, "worse than
    /// direct" -- and `percent_from_basis_points` writes its own minus sign,
    /// so feeding it the raw signed number printed `比直兑低 -13.38%`. A
    /// double negative in the one place a reader is deciding whether a route
    /// is ahead or behind.
    #[test]
    fn a_route_behind_direct_says_so_once() {
        let chinese = versus_direct(
            UiLanguage::Chinese,
            Some(ComparisonDirection::Worse),
            Some(3_451),
            Some(-1_338),
        );
        assert!(
            chinese.contains("比直兑低 13.38%"),
            "the direction is stated by the words, not repeated by the sign: {chinese}"
        );
        assert!(!chinese.contains("-13.38%"), "double negative: {chinese}");

        let english = versus_direct(
            UiLanguage::English,
            Some(ComparisonDirection::Worse),
            Some(3_451),
            Some(-1_338),
        );
        assert!(
            !english.contains("-13.38%"),
            "the English line repeats the minus too: {english}"
        );
        assert!(
            english.contains("13.38% worse than direct"),
            "English has to say which way as words as well: {english}"
        );
    }
}

#[cfg(test)]
mod route_quote_tests {
    use super::*;
    use ptt_trade_domain::{
        Comparator, ExecutionType, QuoteEdge, QuoteEdgeRole, QuoteSide, Ratio, SnapshotRecordStatus,
    };

    const CONTEXT: &str = "route-quote-context";

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    #[allow(clippy::too_many_arguments)]
    fn edge(
        tag: &str,
        edge_id: &str,
        from: &str,
        to: &str,
        side: QuoteSide,
        role: QuoteEdgeRole,
        execution: ExecutionType,
        row: u8,
        rate: (u64, u64),
        stock: u64,
    ) -> MarketEdgeObservation {
        let captured = Utc::now() - chrono::Duration::minutes(1);
        MarketEdgeObservation {
            edge: QuoteEdge {
                edge_id: edge_id.to_owned(),
                snapshot_id: format!("snapshot-{tag}"),
                quote_id: format!("quote-{edge_id}"),
                context_key: CONTEXT.to_owned(),
                from_asset_id: asset(from),
                to_asset_id: asset(to),
                rate: Ratio::from_parts(rate.0, rate.1).expect("rate"),
                source_side: side,
                execution_type: execution,
                role,
                stock,
                original_need_asset_id: asset(to),
                original_have_asset_id: asset(from),
                original_row_index: row,
                comparator: Comparator::Exact,
                user_edited: false,
                machine_confidence_ppm: Some(990_000),
                captured_at: captured,
                confirmed_at: captured,
            },
            snapshot_complete: true,
            record_status: SnapshotRecordStatus::Active,
            record_revision: 1,
            record_reason: None,
        }
    }

    /// One captured panel as the two edges per row the domain builds: a taker
    /// one way and a maker reference back. Only the taker side is fillable
    /// under Instant, so `available` is the book a route can actually walk.
    fn panel(
        tag: &str,
        have: &str,
        need: &str,
        available: &[((u64, u64), u64)],
    ) -> Vec<MarketEdgeObservation> {
        let mut out = Vec::new();
        for (index, (rate, stock)) in available.iter().enumerate() {
            let row = u8::try_from(index).expect("row index");
            out.push(edge(
                tag,
                &format!("{tag}-a{index}-f"),
                have,
                need,
                QuoteSide::Available,
                QuoteEdgeRole::AvailableTaker,
                ExecutionType::Taker,
                row,
                *rate,
                *stock,
            ));
            out.push(edge(
                tag,
                &format!("{tag}-a{index}-r"),
                need,
                have,
                QuoteSide::Available,
                QuoteEdgeRole::AvailableReverseMakerReference,
                ExecutionType::MakerReference,
                row,
                (rate.1, rate.0),
                *stock,
            ));
        }
        out
    }

    /// The divine/chaos book off the 2026-08-23 capture: 10.8 at the front
    /// with 18,576 chaos behind it, then two worse levels. More than one
    /// level is the point -- a sweep of this book blends out to 10.798, and
    /// that is the number this change is taking off the profit line.
    fn direct_book() -> Vec<MarketEdgeObservation> {
        panel(
            "dc",
            "divine-orb",
            "chaos-orb",
            &[((54, 5), 18_576), ((1079, 100), 410), ((539, 50), 619_614)],
        )
    }

    /// A two-hop route priced better than direct -- 12 chaos a divine against
    /// 10.8 -- with almost nothing behind either front row.
    fn thin_but_better() -> Vec<MarketEdgeObservation> {
        let mut out = panel("db", "divine-orb", "bridge-orb", &[((1, 1), 5)]);
        out.extend(panel("bc", "bridge-orb", "chaos-orb", &[((12, 1), 60)]));
        out
    }

    /// A two-hop route priced worse than direct -- 9 chaos a divine -- with a
    /// deep book on both legs. The depth is the point: it must not buy this
    /// route a place on the page.
    fn deep_but_worse() -> Vec<MarketEdgeObservation> {
        let mut out = panel("ds", "divine-orb", "sink-orb", &[((1, 1), 100_000)]);
        out.extend(panel("sc", "sink-orb", "chaos-orb", &[((9, 1), 900_000)]));
        out
    }

    /// Two hops that halve and then multiply back: 11 chaos a divine against
    /// direct's 10.8, but the middle currency costs two divine apiece, so a
    /// tiny ask rounds away at the halfway point.
    fn rounding_trap() -> Vec<MarketEdgeObservation> {
        let mut out = panel("dh", "divine-orb", "half-orb", &[((1, 2), 1_000)]);
        out.extend(panel("hc", "half-orb", "chaos-orb", &[((22, 1), 100_000)]));
        out
    }

    /// The best-priced route on the book -- 12 a divine -- whose middle book
    /// can only pass five divine through, so a large ask strands there and
    /// the engine's liquidity key demotes it at exactly that size.
    fn wide_entry_thin_exit() -> Vec<MarketEdgeObservation> {
        let mut out = panel("dw", "divine-orb", "gate-orb", &[((1, 1), 100_000)]);
        out.extend(panel("wc", "gate-orb", "chaos-orb", &[((12, 1), 60)]));
        out
    }

    /// Slightly ahead of direct -- 11 against 10.8 -- with depth to spare on
    /// both legs, so nothing about it moves with the ask.
    fn deep_and_better() -> Vec<MarketEdgeObservation> {
        let mut out = panel("dt", "divine-orb", "steady-orb", &[((1, 1), 1_000_000)]);
        out.extend(panel(
            "tc",
            "steady-orb",
            "chaos-orb",
            &[((11, 1), 11_000_000)],
        ));
        out
    }

    fn model_at(observations: &[MarketEdgeObservation], holdings: u64) -> ConvertModel {
        convert_model(
            observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            Some(holdings),
            &MarketTuning::default(),
            UiLanguage::English,
            None,
        )
        .expect("convert model")
    }

    fn quotes_at(observations: &[MarketEdgeObservation], holdings: u64) -> Vec<RouteQuote> {
        model_at(observations, holdings)
            .sizes
            .first()
            .expect("one priced size")
            .quotes
            .clone()
    }

    fn find(quotes: &[RouteQuote], hop: &str) -> Option<RouteQuote> {
        quotes
            .iter()
            .find(|quote| quote.route_asset_ids.iter().any(|id| id.as_str() == hop))
            .cloned()
    }

    fn english_report(observations: &[MarketEdgeObservation], holdings: u64) -> String {
        convert_report(
            observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            Some(holdings),
            &MarketTuning::default(),
            UiLanguage::English,
        )
        .expect("report")
        .join("\n")
    }

    /// The list is ordered by the number printed on it. The engine ranks by
    /// what this particular size realized -- stranding and blended price both
    /// move with the ask -- so borrowing its order made the rows shuffle when
    /// the holding changed and parked the direct baseline mid-list. Rate
    /// order is the one order the ask cannot move: best rate first, and the
    /// direct baseline at the bottom as the floor everything above it beat.
    #[test]
    fn the_route_list_is_ordered_by_rate_and_the_ask_cannot_reorder_it() {
        let mut observations = direct_book();
        observations.extend(wide_entry_thin_exit());
        observations.extend(deep_and_better());

        let names = |quotes: &[RouteQuote]| -> Vec<String> {
            quotes
                .iter()
                .map(|quote| {
                    quote
                        .route_asset_ids
                        .iter()
                        .map(|id| id.as_str().to_owned())
                        .collect::<Vec<_>>()
                        .join(">")
                })
                .collect()
        };

        let at_10 = quotes_at(&observations, 10);
        let at_5000 = quotes_at(&observations, 5_000);
        assert_eq!(
            names(&at_10),
            names(&at_5000),
            "the size of the ask reordered the list"
        );
        assert_eq!(
            names(&at_5000),
            vec![
                "divine-orb>gate-orb>chaos-orb".to_owned(),
                "divine-orb>steady-orb>chaos-orb".to_owned(),
                "divine-orb>chaos-orb".to_owned(),
            ],
            "best rate first, the baseline last"
        );
    }

    /// **One route, at most one warning row: where it pinches.**
    ///
    /// The rate row already ends with "the market absorbs N of yours at this
    /// rate, which is less than you asked for". Printing that same sentence
    /// again under every step of every route is how fourteen rows of a
    /// sixteen-row card came to say one thing -- the owner's words: "一整个
    /// 面板全是告诉我吃紧吃紧…我期待的是满屏幕的机会". What the steps can
    /// add that the rate row cannot is *which* step is the narrow one, and
    /// that is one row, not three.
    ///
    /// Choosing the step that earned the verdict also settles an old
    /// complaint: a leg only wearing its neighbour's verdict printed "市面
    /// 挂着 273207，这一趟要吃掉 1150" beside "more than everything listed",
    /// a sentence its own two numbers contradict. The pinch always owns the
    /// sentence it carries.
    #[test]
    fn one_route_shows_one_step_and_it_is_the_one_that_pinches() {
        let mut observations = direct_book();
        observations.extend(panel("dn", "divine-orb", "near-orb", &[((1, 1), 400)]));
        observations.extend(panel("nt", "near-orb", "tight-orb", &[((1, 1), 40)]));
        observations.extend(panel(
            "tc",
            "tight-orb",
            "chaos-orb",
            &[((12, 1), 1_000_000)],
        ));

        let quotes = quotes_at(&observations, 5_000);
        let route = quotes
            .iter()
            .find(|quote| {
                quote
                    .route_asset_ids
                    .iter()
                    .any(|id| id.as_str() == "tight-orb")
            })
            .expect("the route is shown");
        assert_eq!(route.legs.len(), 3, "all three steps are still modelled");

        let pinch = route.pinch().expect("something pinches");
        assert_eq!(pinch.to_asset_id.as_str(), "tight-orb", "{route:?}");
        assert!(
            !pinch.bound_by_next_leg,
            "the row shown is the one that earned its verdict: {pinch:?}"
        );
        assert!(
            pinch.taking > pinch.listed.expect("listed"),
            "so its sentence matches its own two numbers: {pinch:?}"
        );

        let lines = english_report(&observations, 5_000);
        assert_eq!(
            lines
                .lines()
                .filter(|line| line.starts_with("       ") && line.contains(" -> "))
                .count(),
            1,
            "one warning row on the whole card:
{lines}"
        );
    }

    /// Three detours that all leave through the same narrow door.
    ///
    /// Forty hub-orbs are listed against divine and nothing else on these
    /// routes is tight, so all three pinch at `divine-orb -> hub-orb` and
    /// print the same two numbers about the same step.
    fn one_narrow_door() -> Vec<MarketEdgeObservation> {
        let mut out = panel("dh", "divine-orb", "hub-orb", &[((2, 1), 40)]);
        out.extend(panel("hc", "hub-orb", "chaos-orb", &[((6, 1), 10_000_000)]));
        out.extend(panel("ha", "hub-orb", "alpha-orb", &[((3, 1), 1_000_000)]));
        out.extend(panel("ac", "alpha-orb", "chaos-orb", &[((2, 1), 10_000_000)]));
        out.extend(panel("hb", "hub-orb", "beta-orb", &[((3, 1), 1_000_000)]));
        out.extend(panel("bc", "beta-orb", "chaos-orb", &[((2, 1), 10_000_000)]));
        out
    }

    /// **Every detour leaving through the same door is one warning, not one
    /// per detour.**
    ///
    /// The third de-noise cut got a card down to one warning row *per route*.
    /// That is still a wall when the routes share their narrow step, which
    /// they usually do -- a book is hub-and-spoke, so the detours out of a
    /// currency nearly all leave through the same bridge. On the owner's real
    /// card at a holding of 49,500, thirteen routes printed the identical
    /// sentence about 機會石 -> 神聖石 thirteen times.
    ///
    /// Saying it once and naming how many routes it speaks for is strictly
    /// more information than saying it thirteen times, in one line instead of
    /// thirteen.
    #[test]
    fn every_route_leaving_through_the_same_door_is_warned_about_once() {
        let mut observations = direct_book();
        observations.extend(one_narrow_door());

        let lines = english_report(&observations, 5_000);
        assert_eq!(
            lines
                .lines()
                .filter(|line| line.starts_with("       ") && line.contains(" -> "))
                .count(),
            1,
            "the step every detour pinches at is one row, not one per route:
{lines}"
        );
        assert!(
            lines.contains("all 3 routes pinch at this step"),
            "and the row says how many routes it speaks for:
{lines}"
        );
        assert!(
            lines.contains("divine-orb -> hub-orb"),
            "the collapsed row still names the step:
{lines}"
        );

        let route = model_at(&observations, 5_000)
            .sizes
            .into_iter()
            .next()
            .expect("one priced size");
        let (leg, count) = route
            .shared_pinch()
            .expect("every detour pinches at the same step");
        assert_eq!(leg.to_asset_id.as_str(), "hub-orb", "{route:?}");
        assert_eq!(count, 3, "{route:?}");
        assert!(
            route.quotes.len() > count,
            "the clean direct trade is on the card and is not counted: {route:?}"
        );
    }

    /// The collapse must not reach past the case it exists for.
    ///
    /// One route pinching is not a wall, and a count of one would read as a
    /// claim about a set. The same fixture as
    /// `one_route_shows_one_step_and_it_is_the_one_that_pinches`: one pinched
    /// detour beside a direct trade that clears.
    #[test]
    fn one_pinching_route_is_not_collapsed_into_a_claim_about_a_set() {
        let mut observations = direct_book();
        observations.extend(panel("dn", "divine-orb", "near-orb", &[((1, 1), 400)]));
        observations.extend(panel("nt", "near-orb", "tight-orb", &[((1, 1), 40)]));
        observations.extend(panel(
            "tc",
            "tight-orb",
            "chaos-orb",
            &[((12, 1), 1_000_000)],
        ));

        let route = model_at(&observations, 5_000)
            .sizes
            .into_iter()
            .next()
            .expect("one priced size");
        assert!(
            route.shared_pinch().is_none(),
            "only one route pinches, so there is no shared step: {route:?}"
        );

        let lines = english_report(&observations, 5_000);
        assert!(
            !lines.contains("routes pinch at this step"),
            "and the card does not speak for a set of one:
{lines}"
        );
    }

    /// **A page about listing rates does not warn about sliding down tiers,
    /// because listing never slides.**
    ///
    /// The amber band fired whenever a step took more than a quarter of what
    /// was listed and said "会一路吃到深档，均价变差" -- you will eat into
    /// the deep levels and your average price gets worse. That is a taker's
    /// hazard. This page prices what the reader can *list* at, and POE fills
    /// a listing at that rate or better, so the slide the chip warns about
    /// cannot happen to them. It was the bulk of the wall of amber on the
    /// owner's real card -- ten of fourteen step rows -- all of it about a
    /// thing that does not occur.
    ///
    /// What is still worth saying is that the market has not listed enough
    /// to absorb the trip: that one is true whichever side you are on, and
    /// it survives.
    #[test]
    fn taking_a_large_share_of_a_book_that_covers_it_is_not_a_warning() {
        let mut observations = direct_book();
        // Both legs deep enough to fill, but the trip is well over a quarter
        // of each book -- the old amber threshold, twice over.
        observations.extend(panel(
            "dw",
            "divine-orb",
            "wide-orb",
            &[((1, 1), 8), ((1, 1), 8)],
        ));
        observations.extend(panel(
            "wc",
            "wide-orb",
            "chaos-orb",
            &[((12, 1), 100), ((12, 1), 100)],
        ));

        let quotes = quotes_at(&observations, 10);
        let wide = quotes
            .iter()
            .find(|quote| {
                quote
                    .route_asset_ids
                    .iter()
                    .any(|id| id.as_str() == "wide-orb")
            })
            .expect("the route is shown");
        assert!(
            wide.legs
                .iter()
                .all(|leg| leg.verdict == LegTakeVerdict::Covered),
            "listings cover it, so there is nothing to warn about: {wide:?}"
        );

        let lines = english_report(&observations, 10);
        assert!(
            lines.contains("via wide-orb"),
            "the rate still shows: {lines}"
        );
        assert!(
            !lines.contains("sweeps most of what is listed"),
            "a maker never walks into the deep levels: {lines}"
        );
        assert!(
            !lines.contains("divine-orb -> wide-orb"),
            "and with nothing to warn about the step earns no row: {lines}"
        );
    }

    /// **Silence is the all-clear.** A step whose listings cover the trip
    /// used to print a row saying so, with a chip on it saying so again --
    /// on a ten-route card that is thirty rows and thirty chips announcing
    /// that nothing is wrong, and the two or three rows that do matter
    /// disappear into them. The route already carries the one number that
    /// summarises every step it has ("the front rows take N at this rate"),
    /// so a calm step has nothing left to add.
    #[test]
    fn a_step_with_nothing_to_warn_about_prints_no_row() {
        let mut observations = direct_book();
        // A route with depth to spare on both legs: nothing to say about it.
        observations.extend(panel(
            "dq",
            "divine-orb",
            "quiet-orb",
            &[((1, 1), 1_000_000), ((1, 2), 1_000_000)],
        ));
        observations.extend(panel(
            "qc",
            "quiet-orb",
            "chaos-orb",
            &[((11, 1), 9_000_000), ((10, 1), 9_000_000)],
        ));

        let quotes = quotes_at(&observations, 10);
        let calm = quotes
            .iter()
            .find(|quote| {
                quote
                    .route_asset_ids
                    .iter()
                    .any(|id| id.as_str() == "quiet-orb")
            })
            .expect("the route is shown");
        assert!(
            calm.legs
                .iter()
                .all(|leg| leg.verdict == LegTakeVerdict::Covered),
            "fixture should be calm: {calm:?}"
        );

        let lines = english_report(&observations, 10);
        assert!(
            lines.contains("via quiet-orb"),
            "the rate still shows: {lines}"
        );
        assert!(
            !lines.contains("listings cover it"),
            "an all-clear chip is a reminder that nothing is wrong: {lines}"
        );
        assert!(
            !lines.contains("divine-orb -> quiet-orb"),
            "a calm step has nothing to add to the rate above it: {lines}"
        );
    }

    /// **The page has no opinion about the reader's inventory.**
    ///
    /// The clearance block priced a sweep of the whole holding: three
    /// blended tiers, a "size down to N" instruction, the stranded remainder
    /// and its break-even. Every one of them answers a question the market
    /// will not hold still for -- the owner's ruling is that somebody can
    /// add listings while you are converting and the trade goes through
    /// anyway, so a number computed from one snapshot of depth times your
    /// stack is a guess dressed as a forecast. It also crowded out the rate,
    /// which is the only thing on this page that is actually stable.
    ///
    /// What survives is the single depth fact the ruling does ask for,
    /// stated beside the rate it belongs to: how much the market absorbs at
    /// this rate right now.
    #[test]
    fn the_page_prices_rates_and_says_nothing_about_clearing_your_stack() {
        let mut observations = direct_book();
        observations.extend(thin_but_better());
        let lines = english_report(&observations, 5_000);

        for gone in [
            "closed",
            "theoretical",
            "mark-to-mkt",
            "clearance price",
            "size down to",
            "stranded",
            "break even at",
            "no cost basis",
        ] {
            assert!(
                !lines.contains(gone),
                "the clearance block is gone; found {gone:?} in:
{lines}"
            );
        }

        assert!(
            lines.contains("the front rows take 5 divine-orb at this rate"),
            "the one depth fact stays: {lines}"
        );
        assert!(lines.contains("via bridge-orb"), "{lines}");
    }

    /// A detour that only *matches* direct still sorts below it. Direct is
    /// the floor of this list and the row every other row is measured
    /// against, so a route that adds two books' worth of risk for zero
    /// basis points has not earned a place above it -- and the tie-break
    /// that used to decide this was step count ascending, which handed a
    /// one-step direct the win and put the baseline back in the middle of
    /// the list, the exact symptom the rate sort was written to remove.
    #[test]
    fn a_route_that_only_ties_direct_sorts_below_the_baseline() {
        let mut observations = direct_book();
        // 1:2 then 5.4:1 composes to exactly 10.8, the direct front rate.
        observations.extend(panel("dm", "divine-orb", "mirror-orb", &[((1, 2), 10_000)]));
        observations.extend(panel(
            "mc",
            "mirror-orb",
            "chaos-orb",
            &[((108, 5), 1_000_000)],
        ));

        let quotes = quotes_at(&observations, 500);
        let tie = quotes
            .iter()
            .position(|quote| {
                quote
                    .route_asset_ids
                    .iter()
                    .any(|id| id.as_str() == "mirror-orb")
            })
            .expect("the tying route is shown, not hidden");
        let direct = quotes
            .iter()
            .position(|quote| quote.is_direct)
            .expect("direct is always shown");
        assert_eq!(quotes[tie].versus_direct_bps, Some(0), "{quotes:?}");
        assert!(
            direct > tie,
            "direct is the floor and sorts last even against a tie: {quotes:?}"
        );
    }

    /// **How much you hold must not decide which rates you are allowed to
    /// see.**
    ///
    /// The search is handed the reader's holding as its input amount, and
    /// `route.rs` drops any path whose next hop rounds to zero
    /// (`if propagated.quanta == 0 { continue }`). So a bridge currency that
    /// costs more than one unit of what you hold silently deletes every
    /// route through it. On the owner's real book `chaos-orb -> divine-orb`
    /// showed 1 route at a 20-chaos holding, 3 at 50, 7 at 100 and all 10 at
    /// 200 -- and `chaos-orb -> omen-of-whittling` showed *nothing at all*
    /// below 100 chaos, on a pair with eight profitable rates sitting in the
    /// database, under a heading that reads "还没有路线" and offers to go
    /// capture the data again.
    ///
    /// That is the 错杀 ruling 3 exists to prevent, arriving through the
    /// back door: not hidden for pricing worse than direct, just never
    /// enumerated. The rate a route can be listed at does not depend on the
    /// ask, so neither may the list of them.
    #[test]
    fn the_size_of_the_holding_does_not_decide_which_routes_exist() {
        // One bridge orb costs fifty divine, so a small holding cannot buy a
        // whole one and the old search dropped the route entirely.
        let mut observations = direct_book();
        observations.extend(panel("dg", "divine-orb", "gate-orb", &[((1, 50), 400)]));
        observations.extend(panel(
            "gc",
            "gate-orb",
            "chaos-orb",
            &[((600, 1), 1_000_000)],
        ));

        let names = |holdings: u64| -> Vec<String> {
            quotes_at(&observations, holdings)
                .iter()
                .map(|quote| {
                    quote
                        .route_asset_ids
                        .iter()
                        .map(|id| id.as_str().to_owned())
                        .collect::<Vec<_>>()
                        .join(">")
                })
                .collect()
        };

        let reference = names(5_000);
        assert!(
            reference.contains(&"divine-orb>gate-orb>chaos-orb".to_owned()),
            "the bridge route is real at a large ask: {reference:?}"
        );
        for holdings in [1, 7, 20, 49, 200, 5_000] {
            assert_eq!(
                names(holdings),
                reference,
                "the visible routes moved with a holding of {holdings}"
            );
        }
    }

    /// **The invariant this whole change exists to establish.**
    ///
    /// The reader types a size into the game to control how much they list,
    /// not to ask a different question. Ten and five thousand are the same
    /// question about the same book, so the answer -- is this route ahead of
    /// trading direct, and by how much -- has to come out identical. Only the
    /// absolute numbers and the liquidity warnings may move with the ask.
    ///
    /// Every other ruling on this page rests on this one: dropping routes
    /// that price worse than direct is only safe while the sign belongs to
    /// the rates and nothing else.
    #[test]
    fn the_profit_percentage_does_not_move_with_the_size_of_the_ask() {
        let mut observations = direct_book();
        observations.extend(thin_but_better());

        let small = find(&quotes_at(&observations, 10), "bridge-orb").expect("bridge route at 10");
        let large =
            find(&quotes_at(&observations, 5_000), "bridge-orb").expect("bridge route at 5000");

        assert_eq!(
            small.versus_direct_bps, large.versus_direct_bps,
            "the size of the ask changed the profit percentage: {small:?} vs {large:?}"
        );
        assert_eq!(
            small.versus_direct_bps,
            Some(1_111),
            "12 against 10.8 is 11.11% at either size: {small:?}"
        );
        assert_eq!(small.rate, large.rate, "{small:?} vs {large:?}");
        assert_eq!(
            (small.projected_output, large.projected_output),
            (Some(120), Some(60_000)),
            "the absolute numbers are the part that is allowed to scale"
        );
    }

    /// A rate worth having behind a book that cannot fill the ask is exactly
    /// the case that must never be hidden: the currency may be the league's
    /// store of value and the program has no way to know that. So it is
    /// listed, with its rate, and the shortage is said out loud beside it.
    #[test]
    fn a_better_rate_that_cannot_fill_the_ask_is_still_shown() {
        let mut observations = direct_book();
        observations.extend(thin_but_better());

        let quotes = quotes_at(&observations, 5_000);
        let bridge =
            find(&quotes, "bridge-orb").expect("a route that beats direct is never hidden");
        assert_eq!(bridge.rate.map(RouteRate::text), Some("12 : 1".to_owned()));
        assert_eq!(
            bridge.fillable_input,
            Some(5),
            "five divine is all the front rows hold: {bridge:?}"
        );
        assert!(
            bridge
                .legs
                .iter()
                .any(|leg| leg.verdict == LegTakeVerdict::NotEnoughListed),
            "the shortage has to be stated: {bridge:?}"
        );

        let lines = english_report(&observations, 5_000);
        assert!(lines.contains("via bridge-orb"), "{lines}");
        assert!(
            lines.contains("the front rows take 5 divine-orb at this rate"),
            "{lines}"
        );
        assert!(lines.contains("your ask is larger than that"), "{lines}");
    }

    /// Nine chaos a divine is nine chaos a divine however many of them sit on
    /// the shelf. A route the reader would come out behind on is not an
    /// opportunity at any size, so it does not go on the page -- and the
    /// depth behind it must not talk it back on.
    #[test]
    fn a_route_that_prices_worse_than_direct_is_not_listed() {
        let mut observations = direct_book();
        observations.extend(deep_but_worse());

        let quotes = quotes_at(&observations, 5_000);
        assert!(
            find(&quotes, "sink-orb").is_none(),
            "a worse rate stays off the page however deep its book: {quotes:?}"
        );
        assert!(
            quotes.iter().any(|quote| quote.is_direct),
            "the baseline is always shown: {quotes:?}"
        );
    }

    /// The common case, and it is a conclusion rather than a fault: this book
    /// simply has nothing better than trading straight across. Saying "no
    /// route" there would read as a broken page.
    #[test]
    fn when_nothing_beats_direct_the_page_says_so() {
        let mut observations = direct_book();
        observations.extend(deep_but_worse());

        let size = model_at(&observations, 5_000)
            .sizes
            .first()
            .cloned()
            .expect("one priced size");
        assert_eq!(size.quotes.len(), 1, "{:?}", size.quotes);
        assert!(size.quotes[0].is_direct, "{:?}", size.quotes);
        assert!(size.direct_is_the_only_one);

        let english = english_report(&observations, 5_000);
        assert!(english.contains("no route beats going direct"), "{english}");
        assert!(
            !english.contains("no route from divine-orb"),
            "a conclusion must not read as a failure: {english}"
        );

        let chinese = convert_report(
            &observations,
            CONTEXT,
            &asset("divine-orb"),
            &asset("chaos-orb"),
            Some(5_000),
            &MarketTuning::default(),
            UiLanguage::Chinese,
        )
        .expect("report")
        .join("\n");
        assert!(chinese.contains("没有比直兑更好的路线"), "{chinese}");
    }

    /// A row that says "1.85% better" and shows a smaller number than the row
    /// above it is worse than a row that is approximate.
    ///
    /// Found on the live book at a size of three: a three-hop route 3.06%
    /// ahead on rate projected fewer chaos than direct, because rounding each
    /// hop down to a whole orb ate more than the edge was worth. The
    /// projection is now one multiply and one floor over the composed rate,
    /// which cannot fall below a worse rate's.
    #[test]
    fn a_better_rate_never_projects_a_smaller_number() {
        let mut observations = direct_book();
        observations.extend(rounding_trap());

        let quotes = quotes_at(&observations, 3);
        let direct = quotes
            .iter()
            .find(|quote| quote.is_direct)
            .expect("direct")
            .clone();
        let bridged = find(&quotes, "half-orb").expect("a route ahead on rate is shown");

        assert_eq!(bridged.rate.map(RouteRate::text), Some("11 : 1".to_owned()));
        assert_eq!(bridged.versus_direct_bps, Some(185));
        assert_eq!(
            (bridged.projected_output, direct.projected_output),
            (Some(33), Some(32)),
            "flooring each hop separately made this 22 against direct's 32: {bridged:?}"
        );
        assert_eq!(bridged.delta_output, Some(1));
    }

    /// The direct row against the real reading it was modelled on: 10.8 at
    /// the front, 18,576 chaos behind it, and 1,720 divine that can be pushed
    /// through before the price moves off it. Not the 10.798 a five-level
    /// sweep blends out to -- that one is a clearance price and lives under
    /// its own heading further down the card.
    #[test]
    fn the_direct_row_prices_the_front_of_its_book() {
        let quotes = quotes_at(&direct_book(), 5_000);
        let direct = quotes.iter().find(|quote| quote.is_direct).expect("direct");
        assert_eq!(
            direct.rate.map(RouteRate::text),
            Some("10.8 : 1".to_owned())
        );
        assert_eq!(direct.fillable_input, Some(1_720));
        assert_eq!(direct.projected_output, Some(54_000));
        assert_eq!(
            direct.versus_direct_bps,
            Some(0),
            "the baseline is level with itself: {direct:?}"
        );
    }
}

#[cfg(test)]
mod route_walk_tests {
    use super::*;
    use ptt_trade_domain::Ratio;

    fn asset(id: &str) -> MarketAssetId {
        MarketAssetId::try_new(id).expect("asset id")
    }

    /// One saved leg: `rate` as target-per-source, `capacity` in the leg's
    /// own input units, `listed` in what the leg buys.
    fn leg(
        from: &str,
        to: &str,
        rate: (u64, u64),
        capacity: u64,
        listed: Option<u64>,
    ) -> RouteLegBook {
        RouteLegBook {
            from_asset_id: asset(from),
            to_asset_id: asset(to),
            rate: Some(Ratio::from_parts(rate.0, rate.1).expect("rate")),
            front_capacity: Some(capacity),
            listed,
            single_listing: false,
        }
    }

    /// The ruling in one assertion: the ask scales the output linearly and
    /// the rate does not move at all — the walk is a ratio, not a parcel.
    #[test]
    fn the_ask_scales_the_output_but_never_the_rate() {
        let legs = vec![
            leg("a", "b", (2, 1), 1_000, Some(10_000)),
            leg("b", "c", (3, 1), 1_000, Some(10_000)),
        ];
        let small = walk_route(&legs, 10);
        let large = walk_route(&legs, 5_000);
        assert_eq!(small.projected_output, Some(60));
        assert_eq!(large.projected_output, Some(30_000));
        assert_eq!(
            (small.rate, large.rate),
            (large.rate, small.rate),
            "ten and five thousand walk the same rate: {small:?}"
        );
        assert_eq!(small.rate.map(RouteRate::text), Some("6 : 1".to_owned()));
    }

    /// The absorbable size is the thinnest front row, restated in the
    /// reader's own asset: a 50-unit row in the middle currency is 25 units
    /// of a source that doubles on the way in.
    #[test]
    fn fillable_is_the_thinnest_row_in_the_readers_own_units() {
        let legs = vec![
            leg("a", "b", (2, 1), 100, Some(10_000)),
            leg("b", "c", (3, 1), 50, Some(10_000)),
        ];
        let walk = walk_route(&legs, 10);
        assert_eq!(walk.fillable_input, Some(25));
    }

    /// Every step is measured against the ask through the prefix of front
    /// rates — a big ask reads big on every leg, and the leg asked for more
    /// than its book is the one the pinch names.
    #[test]
    fn the_leg_asked_for_more_than_its_book_is_the_pinch() {
        let legs = vec![
            leg("a", "b", (2, 1), 1_000, Some(100_000)),
            leg("b", "c", (3, 1), 1_000, Some(500)),
        ];
        let walk = walk_route(&legs, 1_000);
        assert_eq!(walk.legs[1].taking, 6_000, "1000 × 2 × 3 lands on leg two");
        assert_eq!(walk.legs[1].verdict, LegTakeVerdict::NotEnoughListed);
        let pinch = walk.pinch().expect("the short leg is the pinch");
        assert_eq!(pinch.to_asset_id, asset("c"));
    }

    /// The middle-currency rule holds here exactly as it does on the Convert
    /// page: the leg that buys the middle currency wears the tighter verdict
    /// of the book it is spent into.
    #[test]
    fn a_middle_currency_wears_the_tighter_of_its_two_books() {
        let legs = vec![
            leg("a", "b", (2, 1), 1_000, Some(100_000)),
            leg("b", "c", (3, 1), 1_000, Some(500)),
        ];
        let walk = walk_route(&legs, 1_000);
        assert_eq!(walk.legs[0].verdict, LegTakeVerdict::NotEnoughListed);
        assert!(walk.legs[0].bound_by_next_leg);
    }

    /// A leg with no priced front row breaks the walk from itself onward
    /// rather than inventing a number — and one unknown row makes the whole
    /// absorbable figure unknown, because "the thinnest of the rows I could
    /// see" is not the thinnest row.
    #[test]
    fn an_unpriced_leg_breaks_the_walk_instead_of_inventing_a_number() {
        let legs = vec![
            leg("a", "b", (2, 1), 1_000, Some(10_000)),
            RouteLegBook {
                from_asset_id: asset("b"),
                to_asset_id: asset("c"),
                rate: None,
                front_capacity: None,
                listed: None,
                single_listing: false,
            },
        ];
        let walk = walk_route(&legs, 100);
        assert_eq!(walk.projected_output, None);
        assert_eq!(walk.fillable_input, None);
        assert_eq!(walk.legs[1].verdict, LegTakeVerdict::NoListings);
    }

    /// A loop hands back the currency it started from, so the walk needs no
    /// special case for it: the closing leg is a leg like any other and the
    /// projection lands back in the start asset.
    #[test]
    fn a_loop_projects_back_into_its_own_start_asset() {
        let legs = vec![
            leg("a", "b", (3, 1), 10_000, Some(100_000)),
            leg("b", "c", (2, 1), 10_000, Some(100_000)),
            leg("c", "a", (1, 5), 10_000, Some(100_000)),
        ];
        let walk = walk_route(&legs, 100);
        assert_eq!(
            walk.projected_output,
            Some(120),
            "3 × 2 × 1/5 = 6/5, so 100 comes back as 120"
        );
    }
}
