//! Time of day and season (PLAN.md §8 item 8). Pure: the window reads the
//! local clock and passes it in. Nothing here runs on a timer; the mood is
//! worked out again whenever a Claude event arrives, and Clawd only picks
//! idle extras within minutes of one.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// 0–23.
    pub hour: u8,
    /// 0 = Sunday.
    pub weekday: u8,
    /// 1–12.
    pub month: u8,
}

impl Clock {
    /// `hour,weekday,month`, as `--send __clock --tool` gives it.
    pub fn parse(s: &str) -> Option<Clock> {
        let mut parts = s.split(',').map(|p| p.trim().parse::<u8>().ok());
        let (hour, weekday, month) = (parts.next()??, parts.next()??, parts.next()??);
        (hour < 24 && weekday < 7 && (1..=12).contains(&month)).then_some(Clock { hour, weekday, month })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mood {
    /// The sleepy and asleep delays, as a percentage of the normal ones.
    pub drowsy_pct: u32,
    /// An animation that sometimes replaces an idle fidget or a yawn, and how
    /// often, in percent.
    pub treat: Option<(&'static str, u64)>,
    /// A frame worn on the head.
    pub hat: Option<&'static str>,
    pub mug: bool,
}

impl Mood {
    pub const ORDINARY: Mood = Mood { drowsy_pct: 100, treat: None, hat: None, mug: false };
}

pub fn at(c: Clock) -> Mood {
    const FRIDAY: u8 = 5;
    let weekend = c.weekday == 0 || c.weekday == 6;
    let (drowsy_pct, treat) = match c.hour {
        1..=6 => (40, None),
        22 | 23 | 0 => (60, Some(("dazed", 10))),
        _ if weekend => (50, Some(("heart", 15))),
        7 | 8 => (70, Some(("wake", 25))),
        12 => (100, Some(("heart", 20))),
        15.. if c.weekday == FRIDAY => (100, Some(("celebrate", 20))),
        _ => (100, None),
    };
    let hat = if c.hour >= 23 || c.hour < 5 {
        Some("hat_nightcap")
    } else if c.month == 12 {
        Some("hat_december")
    } else {
        None
    };
    Mood { drowsy_pct, treat, hat, mug: (7..=10).contains(&c.hour) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TUESDAY: u8 = 2;

    fn tuesday(hour: u8) -> Mood {
        at(Clock { hour, weekday: TUESDAY, month: 6 })
    }

    #[test]
    fn an_ordinary_afternoon_is_ordinary() {
        assert_eq!(tuesday(14), Mood::ORDINARY);
    }

    #[test]
    fn nights_and_early_mornings_get_sleepy_sooner() {
        assert_eq!(tuesday(3).drowsy_pct, 40);
        assert_eq!(tuesday(23).drowsy_pct, 60);
        assert_eq!(tuesday(23).treat.unwrap().0, "dazed");
        assert_eq!(tuesday(8).drowsy_pct, 70);
        assert_eq!(tuesday(8).treat.unwrap().0, "wake");
    }

    #[test]
    fn lunch_friday_and_weekends_have_their_treats() {
        assert_eq!(tuesday(12).treat.unwrap().0, "heart");
        assert_eq!(at(Clock { hour: 14, weekday: 5, month: 6 }).treat, None, "not Friday afternoon yet");
        assert_eq!(at(Clock { hour: 16, weekday: 5, month: 6 }).treat.unwrap().0, "celebrate");
        for weekday in [0, 6] {
            let m = at(Clock { hour: 14, weekday, month: 6 });
            assert_eq!((m.drowsy_pct, m.treat.unwrap().0), (50, "heart"));
        }
    }

    #[test]
    fn hats_and_the_morning_mug() {
        let december = |hour| at(Clock { hour, weekday: TUESDAY, month: 12 }).hat;
        assert_eq!(december(14), Some("hat_december"));
        assert_eq!(december(23), Some("hat_nightcap"), "the night-cap wins at night");
        assert_eq!(tuesday(14).hat, None);
        assert_eq!(tuesday(2).hat, Some("hat_nightcap"));
        assert_eq!(tuesday(5).hat, None);
        assert!(tuesday(7).mug && tuesday(10).mug);
        assert!(!tuesday(6).mug && !tuesday(11).mug);
    }

    #[test]
    fn every_treat_and_hat_exists_and_treats_fit_a_one_shot() {
        let s = crate::sprite::builtin();
        for hour in 0..24 {
            for weekday in 0..7 {
                let m = at(Clock { hour, weekday, month: 12 });
                if let Some((name, pct)) = m.treat {
                    assert!(s.anim(name).is_some_and(|a| !a.looping), "{name}");
                    assert!(pct <= 100);
                }
                if let Some(hat) = m.hat {
                    assert!(s.frame(hat).is_some(), "{hat}");
                }
                assert!((1..=100).contains(&m.drowsy_pct));
            }
        }
    }

    #[test]
    fn clock_from_the_debug_command() {
        assert_eq!(Clock::parse("23, 2,12"), Some(Clock { hour: 23, weekday: 2, month: 12 }));
        for bad in ["", "23,2", "24,2,12", "1,7,1", "1,1,13", "a,b,c"] {
            assert_eq!(Clock::parse(bad), None, "{bad}");
        }
    }
}
