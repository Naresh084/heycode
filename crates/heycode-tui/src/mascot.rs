//! HeyCode's local, decorative companion. Never sends or retains message text.

/// Number of terminal cells reserved by every mascot pose.
pub const WIDTH: u16 = 14;
const REACTION_TICKS: u8 = 16;

/// A short greeting or acknowledgement, independent of agent authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// Raise a paw in greeting.
    Wave,
    /// Blink one eye.
    Wink,
    /// A small seated bounce.
    Bounce,
    /// A contented acknowledgement.
    Purr,
    /// Attend to a new request.
    Listen,
    /// A quiet failure acknowledgement.
    Concern,
}

/// Bounded animation state; rapid clicks never queue animations.
#[derive(Debug, Default)]
pub struct Companion {
    reaction: Option<Reaction>,
    tick: u8,
    clicks: u8,
}

impl Companion {
    /// Start one finite reaction.
    pub fn react(&mut self, reaction: Reaction) {
        self.reaction = Some(reaction);
        self.tick = 0;
    }

    /// Alternate greetings without changing the composer or running a tool.
    pub fn click(&mut self) {
        if self.is_animating() {
            return;
        }
        let reaction = [
            Reaction::Wave,
            Reaction::Wink,
            Reaction::Bounce,
            Reaction::Purr,
        ][usize::from(self.clicks % 4)];
        self.clicks = self.clicks.wrapping_add(1);
        self.react(reaction);
    }

    /// Classify only a small prefix locally; never store the user's message.
    pub fn message(&mut self, text: &str) {
        let word = text.split_whitespace().next().unwrap_or_default();
        let word = word.trim_matches(|c: char| !c.is_alphabetic());
        let reaction = if ["hi", "hey", "hello", "helloheycode"]
            .iter()
            .any(|candidate| word.eq_ignore_ascii_case(candidate))
        {
            Reaction::Wave
        } else if ["thanks", "thank", "cheers"]
            .iter()
            .any(|candidate| word.eq_ignore_ascii_case(candidate))
        {
            Reaction::Purr
        } else {
            Reaction::Listen
        };
        self.react(reaction);
    }

    /// Advance at 120 ms intervals; return to the operational pose automatically.
    pub fn advance(&mut self) {
        if self.reaction.is_some() {
            self.tick += 1;
            if self.tick >= REACTION_TICKS {
                self.reaction = None;
                self.tick = 0;
            }
        }
    }

    /// Whether the short animation needs a fast redraw timer.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.reaction.is_some()
    }

    /// Current short reaction, for deterministic UI diagnostics and tests.
    #[must_use]
    pub fn reaction(&self) -> Option<Reaction> {
        self.reaction
    }

    /// Four fixed-width pixel rows. Waiting always outranks decoration.
    #[must_use]
    pub fn rows(&self, idle: usize, busy: bool, waiting: bool, animate: bool) -> [String; 4] {
        let phase = usize::from(self.tick / 2);
        let reaction = animate.then_some(self.reaction).flatten();
        let eyes = if waiting {
            "? ᴗ ?"
        } else {
            match reaction {
                Some(Reaction::Wave | Reaction::Bounce) => "◠ ᴗ ◠",
                Some(Reaction::Wink) if phase % 3 == 1 => "─ ᴗ ●",
                Some(Reaction::Purr) => "─ ᴗ ─",
                Some(Reaction::Concern) => "· ᴖ ·",
                Some(Reaction::Listen) if phase % 3 == 1 => "● ᴗ ◕",
                _ if animate && !busy && idle % 8 == 5 => "─ ᴗ ─",
                _ if animate && busy && idle % 4 == 2 => "◕ ᴗ ●",
                _ => "● ᴗ ●",
            }
        };
        let wave = reaction == Some(Reaction::Wave) && !waiting;
        let bounce = reaction == Some(Reaction::Bounce) && phase % 2 == 1 && !waiting;
        let tail = if wave {
            if phase % 2 == 0 { " ▗▀" } else { " ▝▖" }
        } else if animate && (idle % 8 == 4 || reaction == Some(Reaction::Purr)) {
            " ▖ "
        } else {
            "   "
        };
        let rows = [
            if bounce {
                " ▟▙     ▟▙ ˖".to_owned()
            } else {
                " ▟▙     ▟▙".to_owned()
            },
            " ▟██▅▅▅██▙".to_owned(),
            format!(" █ {eyes} █{tail}"),
            if bounce {
                " ▀▙▃▟ ▙▃▟▀".to_owned()
            } else {
                " ▀▙▃▃▃▃▃▟▀▟▌".to_owned()
            },
        ];
        rows.map(|row| {
            use unicode_width::UnicodeWidthStr;
            let padding = usize::from(WIDTH).saturating_sub(row.width());
            format!("{row}{}", " ".repeat(padding))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_poses_fit_the_same_cell_box_and_settle() {
        for reaction in [
            Reaction::Wave,
            Reaction::Wink,
            Reaction::Bounce,
            Reaction::Purr,
            Reaction::Listen,
            Reaction::Concern,
        ] {
            let mut companion = Companion::default();
            companion.react(reaction);
            for frame in 0..24 {
                for busy in [false, true] {
                    for waiting in [false, true] {
                        for row in companion.rows(frame, busy, waiting, true) {
                            assert_eq!(
                                unicode_width::UnicodeWidthStr::width(row.as_str()),
                                usize::from(WIDTH)
                            );
                        }
                    }
                }
                companion.advance();
            }
            assert!(!companion.is_animating());
        }
    }

    #[test]
    fn clicks_are_bounded_and_messages_are_local_reactions() {
        let mut companion = Companion::default();
        companion.click();
        assert_eq!(companion.reaction(), Some(Reaction::Wave));
        for _ in 0..REACTION_TICKS {
            companion.click();
            companion.advance();
        }
        assert!(!companion.is_animating());
        companion.click();
        assert_eq!(companion.reaction(), Some(Reaction::Wink));
        companion.message("Thanks! that worked");
        assert_eq!(companion.reaction(), Some(Reaction::Purr));
        companion.message("fix the failing tests");
        assert_eq!(companion.reaction(), Some(Reaction::Listen));
    }

    #[test]
    fn waiting_and_reduced_motion_override_play() {
        let mut companion = Companion::default();
        companion.click();
        assert!(companion.rows(0, false, true, true)[2].contains("? ᴗ ?"));
        let still = companion.rows(0, false, false, false);
        for frame in 0..24 {
            companion.advance();
            assert_eq!(companion.rows(frame, false, false, false), still);
        }
    }
}
