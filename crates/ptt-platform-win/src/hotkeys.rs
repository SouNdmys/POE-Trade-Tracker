use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::{NativeWindowHandle, PlatformError};

const WM_HOTKEY: u32 = 0x0312;
const VK_F10: u32 = 0x79;
const VK_F11: u32 = 0x7A;
const VK_F12: u32 = 0x7B;
const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

// These preserve the stable .NET identifiers ('POE', 'POF', and 'POG').
const ACKNOWLEDGE_ID: i32 = 0x50_4F_45;
const SELECT_REGION_ID: i32 = 0x50_4F_46;
const START_ID: i32 = 0x50_4F_47;
const TOGGLE_HUD_ID: i32 = 0x50_4F_48;

/// Semantic action emitted by the three stable global shortcuts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HotKeyAction {
    StartMonitoring,
    ToggleHud,
    SelectRegion,
    StopOrAcknowledge,
}

impl HotKeyAction {
    const ALL: [Self; 4] = [
        Self::StartMonitoring,
        Self::ToggleHud,
        Self::SelectRegion,
        Self::StopOrAcknowledge,
    ];

    const fn identifier(self) -> i32 {
        match self {
            Self::StartMonitoring => START_ID,
            Self::ToggleHud => TOGGLE_HUD_ID,
            Self::SelectRegion => SELECT_REGION_ID,
            Self::StopOrAcknowledge => ACKNOWLEDGE_ID,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::StartMonitoring => 0,
            Self::ToggleHud => 1,
            Self::SelectRegion => 2,
            Self::StopOrAcknowledge => 3,
        }
    }
}

/// RegisterHotKey modifier bits, excluding the always-applied `MOD_NOREPEAT`.
#[derive(Clone, Copy, Default, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct HotKeyModifiers(u32);

impl HotKeyModifiers {
    pub const ALT: Self = Self(0x0001);
    pub const CONTROL: Self = Self(0x0002);
    pub const SHIFT: Self = Self(0x0004);
    pub const WINDOWS: Self = Self(0x0008);
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for HotKeyModifiers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = formatter.debug_set();
        if self.0 & Self::CONTROL.0 != 0 {
            list.entry(&"CONTROL");
        }
        if self.0 & Self::SHIFT.0 != 0 {
            list.entry(&"SHIFT");
        }
        if self.0 & Self::ALT.0 != 0 {
            list.entry(&"ALT");
        }
        if self.0 & Self::WINDOWS.0 != 0 {
            list.entry(&"WINDOWS");
        }
        list.finish()
    }
}

impl std::ops::BitOr for HotKeyModifiers {
    type Output = Self;

    fn bitor(self, right: Self) -> Self::Output {
        self.union(right)
    }
}

/// A normalized global shortcut binding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HotKeyBinding {
    pub modifiers: HotKeyModifiers,
    pub virtual_key: u32,
}

impl HotKeyBinding {
    #[must_use]
    pub const fn new(modifiers: HotKeyModifiers, virtual_key: u32) -> Self {
        Self {
            modifiers,
            virtual_key,
        }
    }
}

/// The three start-monitoring options shipped in .NET 1.0.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StartMonitoringHotKey {
    #[default]
    ControlShiftF10,
    ControlAltF10,
    AltF10,
}

impl StartMonitoringHotKey {
    pub const DEFAULT_SETTING_VALUE: &'static str = "Ctrl+Shift+F10";
    pub const OPTIONS: [Self; 3] = [Self::ControlShiftF10, Self::ControlAltF10, Self::AltF10];

    /// Parses settings case-insensitively and falls back to the stable default.
    #[must_use]
    pub fn parse_or_default(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim) else {
            return Self::default();
        };
        Self::OPTIONS
            .into_iter()
            .find(|candidate| candidate.setting_value().eq_ignore_ascii_case(value))
            .unwrap_or_default()
    }

    #[must_use]
    pub const fn setting_value(self) -> &'static str {
        match self {
            Self::ControlShiftF10 => Self::DEFAULT_SETTING_VALUE,
            Self::ControlAltF10 => "Ctrl+Alt+F10",
            Self::AltF10 => "Alt+F10",
        }
    }

    #[must_use]
    pub const fn binding(self) -> HotKeyBinding {
        let modifiers = match self {
            Self::ControlShiftF10 => HotKeyModifiers::CONTROL.union(HotKeyModifiers::SHIFT),
            Self::ControlAltF10 => HotKeyModifiers::CONTROL.union(HotKeyModifiers::ALT),
            Self::AltF10 => HotKeyModifiers::ALT,
        };
        HotKeyBinding::new(modifiers, VK_F10)
    }
}

impl fmt::Display for StartMonitoringHotKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.setting_value())
    }
}

/// Complete stable shortcut configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HotKeyConfig {
    pub start: StartMonitoringHotKey,
    pub hud: HudToggleHotKey,
}

impl HotKeyConfig {
    #[must_use]
    pub const fn binding(self, action: HotKeyAction) -> HotKeyBinding {
        match action {
            HotKeyAction::StartMonitoring => self.start.binding(),
            HotKeyAction::ToggleHud => self.hud.binding(),
            HotKeyAction::SelectRegion => HotKeyBinding::new(
                HotKeyModifiers::CONTROL.union(HotKeyModifiers::SHIFT),
                VK_F11,
            ),
            HotKeyAction::StopOrAcknowledge => HotKeyBinding::new(
                HotKeyModifiers::CONTROL.union(HotKeyModifiers::SHIFT),
                VK_F12,
            ),
        }
    }
}

/// The supported bindings for showing and hiding the HUD.
///
/// A fixed set for the same reason the watch toggle has one: a free-text
/// binding lets a settings file ask for a combination that cannot be
/// registered, and the user then has a key that silently does nothing.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum HudToggleHotKey {
    #[default]
    AltF11,
    ControlAltF11,
    ControlShiftF11,
}

impl HudToggleHotKey {
    pub const DEFAULT_SETTING_VALUE: &'static str = "Alt+F11";
    pub const OPTIONS: [Self; 3] = [Self::AltF11, Self::ControlAltF11, Self::ControlShiftF11];

    /// Parses settings case-insensitively and falls back to the stable default.
    #[must_use]
    pub fn parse_or_default(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim) else {
            return Self::default();
        };
        Self::OPTIONS
            .into_iter()
            .find(|candidate| candidate.setting_value().eq_ignore_ascii_case(value))
            .unwrap_or_default()
    }

    #[must_use]
    pub const fn setting_value(self) -> &'static str {
        match self {
            Self::AltF11 => Self::DEFAULT_SETTING_VALUE,
            Self::ControlAltF11 => "Ctrl+Alt+F11",
            Self::ControlShiftF11 => "Ctrl+Shift+F11",
        }
    }

    #[must_use]
    pub const fn binding(self) -> HotKeyBinding {
        let modifiers = match self {
            Self::AltF11 => HotKeyModifiers::ALT,
            Self::ControlAltF11 => HotKeyModifiers::CONTROL.union(HotKeyModifiers::ALT),
            Self::ControlShiftF11 => HotKeyModifiers::CONTROL.union(HotKeyModifiers::SHIFT),
        };
        HotKeyBinding::new(modifiers, VK_F11)
    }
}

/// Destination that receives `WM_HOTKEY`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotKeyTarget {
    /// The queue of the thread performing registration.
    CurrentThread,
    /// A live native top-level window.
    Window(NativeWindowHandle),
}

/// Classification of a registration failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HotKeyErrorKind {
    Conflict,
    Platform(PlatformError),
}

/// Explicit per-action global shortcut failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotKeyError {
    pub action: HotKeyAction,
    pub binding: HotKeyBinding,
    pub kind: HotKeyErrorKind,
}

impl fmt::Display for HotKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            HotKeyErrorKind::Conflict => write!(
                formatter,
                "the {:?} global shortcut is already in use",
                self.action
            ),
            HotKeyErrorKind::Platform(error) => write!(
                formatter,
                "could not register the {:?} global shortcut: {error}",
                self.action
            ),
        }
    }
}

impl std::error::Error for HotKeyError {}

/// Owns `RegisterHotKey` registrations and releases them on drop.
///
/// The type is deliberately thread-bound: thread-queue registrations must be
/// unregistered on the same UI/message-loop thread that created them.
#[derive(Debug)]
pub struct HotKeyManager {
    target: HotKeyTarget,
    config: HotKeyConfig,
    registered: [bool; HotKeyAction::ALL.len()],
    _thread_bound: PhantomData<Rc<()>>,
}

impl HotKeyManager {
    /// Registers all three shortcuts for the current thread message queue.
    pub fn register_for_current_thread(config: HotKeyConfig) -> Result<Self, HotKeyError> {
        Self::register_all(HotKeyTarget::CurrentThread, config)
    }

    /// Registers all three shortcuts for a native window.
    pub fn register_for_window(
        window: NativeWindowHandle,
        config: HotKeyConfig,
    ) -> Result<Self, HotKeyError> {
        Self::register_all(HotKeyTarget::Window(window), config)
    }

    /// Creates an empty manager so callers can register each action and retain
    /// the remaining shortcuts when one is occupied by another application.
    #[must_use]
    pub fn unregistered(target: HotKeyTarget, config: HotKeyConfig) -> Self {
        Self {
            target,
            config,
            registered: [false; HotKeyAction::ALL.len()],
            _thread_bound: PhantomData,
        }
    }

    fn register_all(target: HotKeyTarget, config: HotKeyConfig) -> Result<Self, HotKeyError> {
        let mut manager = Self::unregistered(target, config);
        for action in HotKeyAction::ALL {
            if let Err(error) = manager.register(action) {
                manager.unregister_all();
                return Err(error);
            }
        }
        Ok(manager)
    }

    /// Registers one action. Re-registering an already active action is a no-op.
    pub fn register(&mut self, action: HotKeyAction) -> Result<(), HotKeyError> {
        if self.registered[action.index()] {
            return Ok(());
        }
        let binding = self.config.binding(action);
        platform_register(self.target, action.identifier(), binding).map_err(|error| {
            let kind = if error.win32_code() == Some(ERROR_HOTKEY_ALREADY_REGISTERED) {
                HotKeyErrorKind::Conflict
            } else {
                HotKeyErrorKind::Platform(error)
            };
            HotKeyError {
                action,
                binding,
                kind,
            }
        })?;
        self.registered[action.index()] = true;
        Ok(())
    }

    /// Replaces the configurable F10 shortcut. Like the 1.0 UI, a conflict
    /// leaves start monitoring unregistered while F11/F12 remain available.
    pub fn reconfigure_start(&mut self, start: StartMonitoringHotKey) -> Result<(), HotKeyError> {
        if self.config.start == start && self.is_registered(HotKeyAction::StartMonitoring) {
            return Ok(());
        }
        self.unregister(HotKeyAction::StartMonitoring);
        self.config.start = start;
        self.register(HotKeyAction::StartMonitoring)
    }

    pub fn unregister(&mut self, action: HotKeyAction) {
        if self.registered[action.index()] {
            platform_unregister(self.target, action.identifier());
            self.registered[action.index()] = false;
        }
    }

    pub fn unregister_all(&mut self) {
        for action in HotKeyAction::ALL {
            self.unregister(action);
        }
    }

    #[must_use]
    pub const fn config(&self) -> HotKeyConfig {
        self.config
    }

    #[must_use]
    pub const fn is_registered(&self, action: HotKeyAction) -> bool {
        self.registered[action.index()]
    }

    /// Maps a window message to an active semantic action.
    #[must_use]
    pub fn action_for_message(&self, message: u32, wparam: usize) -> Option<HotKeyAction> {
        if message != WM_HOTKEY {
            return None;
        }
        HotKeyAction::ALL.into_iter().find(|action| {
            self.registered[action.index()] && wparam == action.identifier() as usize
        })
    }
}

impl Drop for HotKeyManager {
    fn drop(&mut self) {
        self.unregister_all();
    }
}

fn platform_register(
    target: HotKeyTarget,
    identifier: i32,
    binding: HotKeyBinding,
) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        crate::win32::register_hot_key(target, identifier, binding)
    }
    #[cfg(not(windows))]
    {
        crate::non_windows::register_hot_key(target, identifier, binding)
    }
}

fn platform_unregister(target: HotKeyTarget, identifier: i32) {
    #[cfg(windows)]
    crate::win32::unregister_hot_key(target, identifier);
    #[cfg(not(windows))]
    crate::non_windows::unregister_hot_key(target, identifier);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_a_slot_and_a_unique_identifier() {
        // The registration array is indexed by `action.index()`. Adding an
        // action without widening the array indexes out of bounds, which is
        // exactly what happened when the HUD toggle was added.
        for action in HotKeyAction::ALL {
            assert!(
                action.index() < HotKeyAction::ALL.len(),
                "{action:?} indexes past the registration array"
            );
        }
        let mut indices: Vec<usize> = HotKeyAction::ALL.iter().map(|a| a.index()).collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(
            indices.len(),
            HotKeyAction::ALL.len(),
            "two actions share a slot, so one would silently unregister the other"
        );

        let mut identifiers: Vec<i32> = HotKeyAction::ALL.iter().map(|a| a.identifier()).collect();
        identifiers.sort_unstable();
        identifiers.dedup();
        assert_eq!(
            identifiers.len(),
            HotKeyAction::ALL.len(),
            "two actions share a RegisterHotKey id"
        );
    }

    #[test]
    fn the_hud_binding_round_trips_through_settings_text() {
        for option in HudToggleHotKey::OPTIONS {
            assert_eq!(
                HudToggleHotKey::parse_or_default(Some(option.setting_value())),
                option
            );
        }
        // Anything outside the supported set falls to the default rather than
        // registering nothing and leaving the user with a dead key.
        assert_eq!(
            HudToggleHotKey::parse_or_default(Some("Ctrl+Alt+Shift+F9")),
            HudToggleHotKey::AltF11
        );
    }

    #[test]
    fn invalid_start_setting_uses_stable_default() {
        assert_eq!(
            StartMonitoringHotKey::parse_or_default(Some("  ctrl+alt+f10 ")),
            StartMonitoringHotKey::ControlAltF10
        );
        assert_eq!(
            StartMonitoringHotKey::parse_or_default(Some("F10")),
            StartMonitoringHotKey::ControlShiftF10
        );
        assert_eq!(
            StartMonitoringHotKey::parse_or_default(None).setting_value(),
            StartMonitoringHotKey::DEFAULT_SETTING_VALUE
        );
    }

    #[test]
    fn fixed_bindings_match_one_point_zero() {
        let config = HotKeyConfig::default();
        assert_eq!(
            config.binding(HotKeyAction::SelectRegion),
            HotKeyBinding::new(HotKeyModifiers::CONTROL | HotKeyModifiers::SHIFT, VK_F11)
        );
        assert_eq!(
            config.binding(HotKeyAction::StopOrAcknowledge),
            HotKeyBinding::new(HotKeyModifiers::CONTROL | HotKeyModifiers::SHIFT, VK_F12)
        );
    }

    #[test]
    fn message_mapping_requires_an_active_registration() {
        let mut manager =
            HotKeyManager::unregistered(HotKeyTarget::CurrentThread, HotKeyConfig::default());
        assert_eq!(
            manager.action_for_message(WM_HOTKEY, START_ID as usize),
            None
        );
        manager.registered[HotKeyAction::StartMonitoring.index()] = true;
        assert_eq!(
            manager.action_for_message(WM_HOTKEY, START_ID as usize),
            Some(HotKeyAction::StartMonitoring)
        );
        assert_eq!(manager.action_for_message(0, START_ID as usize), None);
        // Prevent Drop from invoking the native unregister adapter in this pure test.
        manager.registered = [false; HotKeyAction::ALL.len()];
    }
}
