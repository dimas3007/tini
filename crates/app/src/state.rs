//! Notch display rules. Pure (no Win32) so it is easy to test.

/// What the notch is currently showing.
#[derive(Clone, Debug, PartialEq)]
pub enum Visual {
    /// Fully hidden, e.g. when a fullscreen app or presentation is active.
    Hidden,
    /// Only Nochi's eyes peek out, when the active window occupies the notch area (shy mode).
    Peek,
    /// Small pill: Nochi + remaining tasks. When hovered, shows the next task.
    Pill,
    /// Agent is working: one activity line, two lines when hovered.
    Live,
    /// Pomodoro running or paused: time remaining; with seconds when hovered.
    Focus,
    /// Pill briefly widens to carry one message.
    Glance(String),
    /// Smart chip: one piece of news with a next-step button (link copied, download finished...).
    Chip,
    /// Agent asks for permission: a card with Allow / Deny buttons.
    Permission,
    /// Full panel.
    Expanded,
}

/// Conditions that determine the display.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// Opened via click, hotkey, or tray.
    pub expanded: bool,
    /// The active window covers the whole monitor, or Windows is in presentation mode.
    pub fullscreen: bool,
    /// The active window occupies the top center area of the monitor.
    pub shy: bool,
    /// The cursor is over the notch (after a short delay).
    pub hovering: bool,
    /// A glance message that is still active.
    pub glance: Option<String>,
    /// A smart chip is currently showing.
    pub chip: bool,
    /// An agent is currently working.
    pub live: bool,
    /// A permission request is waiting for an answer.
    pub permission: bool,
    /// Pomodoro is running or paused.
    pub focus: bool,
    /// Idle size "Shy": the pill is replaced by Nochi's eyes peeking out until hovered. Agent, focus, and
    /// other news still show in full.
    pub rest_shy: bool,
}

pub fn decide(env: &Env) -> Visual {
    // The order is the priority. Permission requests come first: the agent is waiting, so its card
    // appears even when the panel is open, a fullscreen app is active, or the notch is shy. Once answered,
    // the display returns to its previous state (e.g. the panel).
    if env.permission {
        return Visual::Permission;
    }
    if env.expanded {
        return Visual::Expanded;
    }
    if env.fullscreen {
        return Visual::Hidden;
    }
    if env.chip {
        return Visual::Chip;
    }
    if let Some(text) = &env.glance {
        return Visual::Glance(text.clone());
    }
    if env.shy && !env.hovering {
        return Visual::Peek;
    }
    // A working agent is only temporary; the countdown resumes as soon as the agent stops.
    if env.live {
        return Visual::Live;
    }
    if env.focus {
        return Visual::Focus;
    }
    if env.rest_shy && !env.hovering {
        return Visual::Peek;
    }
    Visual::Pill
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_pill() {
        assert_eq!(decide(&Env::default()), Visual::Pill);
    }

    #[test]
    fn shy_retracts_to_peek_until_hovered() {
        let mut env = Env { shy: true, live: true, ..Env::default() };
        assert_eq!(decide(&env), Visual::Peek);
        env.hovering = true;
        assert_eq!(decide(&env), Visual::Live);
    }

    #[test]
    fn glance_shows_even_when_shy() {
        let env = Env { shy: true, glance: Some("Selesai".into()), ..Env::default() };
        assert_eq!(decide(&env), Visual::Glance("Selesai".into()));
    }

    #[test]
    fn fullscreen_hides_everything_but_expanded_and_permission() {
        let mut env = Env { fullscreen: true, glance: Some("x".into()), live: true, ..Env::default() };
        assert_eq!(decide(&env), Visual::Hidden);
        env.expanded = true;
        assert_eq!(decide(&env), Visual::Expanded);
        env.permission = true;
        assert_eq!(decide(&env), Visual::Permission);
        env.permission = false;
        assert_eq!(decide(&env), Visual::Expanded);
    }

    #[test]
    fn focus_shows_below_live_and_glance() {
        let mut env = Env { focus: true, ..Env::default() };
        assert_eq!(decide(&env), Visual::Focus);
        env.live = true;
        assert_eq!(decide(&env), Visual::Live);
        env.live = false;
        env.glance = Some("Focus done".into());
        assert_eq!(decide(&env), Visual::Glance("Focus done".into()));
        env.glance = None;
        env.shy = true;
        assert_eq!(decide(&env), Visual::Peek);
    }

    #[test]
    fn chip_shows_over_live_and_shy_but_not_fullscreen() {
        let mut env = Env { chip: true, live: true, shy: true, ..Env::default() };
        assert_eq!(decide(&env), Visual::Chip);
        env.fullscreen = true;
        assert_eq!(decide(&env), Visual::Hidden);
    }

    #[test]
    fn permission_and_panel_cover_a_chip_that_stays() {
        // Prayer reminder waits to be dismissed: the permission card and panel still appear above it, then
        // its chip returns after both are done.
        let mut env = Env { chip: true, ..Env::default() };
        env.permission = true;
        assert_eq!(decide(&env), Visual::Permission);
        env.permission = false;
        assert_eq!(decide(&env), Visual::Chip);
        env.expanded = true;
        assert_eq!(decide(&env), Visual::Expanded);
        env.expanded = false;
        assert_eq!(decide(&env), Visual::Chip);
    }

    #[test]
    fn shy_size_only_replaces_the_idle_pill() {
        let mut env = Env { rest_shy: true, ..Env::default() };
        assert_eq!(decide(&env), Visual::Peek);
        env.hovering = true;
        assert_eq!(decide(&env), Visual::Pill);
        env.hovering = false;
        env.live = true;
        assert_eq!(decide(&env), Visual::Live);
        env.live = false;
        env.focus = true;
        assert_eq!(decide(&env), Visual::Focus);
    }

    #[test]
    fn live_agent_replaces_idle_pill() {
        let env = Env { live: true, ..Env::default() };
        assert_eq!(decide(&env), Visual::Live);
    }
}
