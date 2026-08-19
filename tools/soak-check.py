"""Audits a soak session against invariants the panel itself guarantees.

Reads %LOCALAPPDATA%\\PoeTradeTracker\\market.sqlite and checks every book
captured in a time window. Needs no ground truth: the checks come from what
the widget does, not from what the market did.

Usage:
    python soak-check.py             # everything captured in the last 2 hours
    python soak-check.py 24          # the last 24 hours
    python soak-check.py all         # the whole store
"""

import os
import sqlite3
import sys
from collections import Counter
from datetime import datetime, timedelta, timezone
from fractions import Fraction

DB = os.path.join(os.environ["LOCALAPPDATA"], "PoeTradeTracker", "market.sqlite")

# Each table is sorted, best offer first, so the taker's rate only gets worse
# going down. The available side improves for the seller and the competing side
# for the buyer, hence the opposite directions.
SIDES = {
    "available_taker": ("available", "non-increasing"),
    "competing_maker_reference": ("competing", "non-decreasing"),
}
MAX_ROWS_PER_SIDE = 6


def window_start(argument):
    if argument == "all":
        return None
    hours = float(argument) if argument else 2.0
    return datetime.now(timezone.utc) - timedelta(hours=hours)


def main():
    if not os.path.exists(DB):
        print("no store at", DB)
        print("run a session first — the app writes it on the first accepted book")
        return 1

    since = window_start(sys.argv[1] if len(sys.argv) > 1 else "")
    db = sqlite3.connect(DB)
    if since is None:
        snapshots = list(db.execute(
            "select snapshot_id, have_asset_id, need_asset_id, captured_at "
            "from market_snapshots order by captured_at"))
    else:
        snapshots = list(db.execute(
            "select snapshot_id, have_asset_id, need_asset_id, captured_at "
            "from market_snapshots where captured_at >= ? order by captured_at",
            (since.isoformat(),)))

    if not snapshots:
        print("no books captured in that window")
        return 1

    problems = []
    tables = 0
    rows_total = 0
    pairs = Counter()

    for snapshot_id, have, need, captured in snapshots:
        pairs[f"{have} -> {need}"] += 1
        for role, (side, direction) in SIDES.items():
            rows = list(db.execute(
                "select original_row_index, rate_numerator, rate_denominator, "
                "rate_text, comparator, stock from quote_edges "
                "where snapshot_id = ? and role = ? order by original_row_index",
                (snapshot_id, role)))
            if not rows:
                continue
            tables += 1
            rows_total += len(rows)
            where = f"{captured[:19]}  {have} -> {need}  {side}"

            if len(rows) > MAX_ROWS_PER_SIDE:
                problems.append(
                    f"{where}: {len(rows)} rows, the panel shows at most "
                    f"{MAX_ROWS_PER_SIDE}")

            for index, numerator, denominator, text, comparator, stock in rows:
                if denominator == 0:
                    problems.append(f"{where} row{index}: zero denominator in {text!r}")
                if stock is not None and stock <= 0:
                    problems.append(f"{where} row{index}: stock {stock} in {text!r}")

            # Only the last row may carry a comparator, and only the one its
            # side allows: available aggregates downward, competing upward.
            expected = "less_than" if side == "available" else "greater_than"
            for position, (index, _, _, text, comparator, _) in enumerate(rows):
                last = position == len(rows) - 1
                if comparator == "exact":
                    continue
                if not last:
                    problems.append(
                        f"{where} row{index}: {comparator} on a row that is not "
                        f"the last ({text!r})")
                elif comparator != expected:
                    problems.append(
                        f"{where} row{index}: {comparator} where this side "
                        f"aggregates {expected} ({text!r})")

            values = [(i, Fraction(int(n), int(d)) if d else None, t)
                      for i, n, d, t, _, _ in rows]
            for (index_a, value_a, text_a), (index_b, value_b, text_b) in zip(values, values[1:]):
                if value_a is None or value_b is None:
                    continue
                ok = value_b <= value_a if direction == "non-increasing" else value_b >= value_a
                if not ok:
                    problems.append(
                        f"{where}: row{index_a} {text_a} then row{index_b} "
                        f"{text_b} — the panel does not go back the other way")

    print(f"books      {len(snapshots)}")
    print(f"tables     {tables}")
    print(f"rows       {rows_total}")
    print("pairs      " + ", ".join(f"{pair} x{count}"
                                    for pair, count in pairs.most_common(8)))
    print()
    if problems:
        print(f"SUSPECT READS: {len(problems)}")
        for problem in problems:
            print("  " + problem)
        print()
        print("Each of these is a value the panel cannot have shown. Send this")
        print("output back — the row text says which digits were lost.")
        return 1
    print("clean: every table is in panel order, every aggregate is where it")
    print("belongs, and no row claims a rate its neighbours contradict.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
