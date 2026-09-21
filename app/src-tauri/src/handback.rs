//! Windows plays into our virtual device only while the engine is there to pick it up.
//!
//! Whenever the engine goes away, the Windows default goes back to the output the user was
//! listening on; when the engine comes back, so does the default. "Goes away" has three shapes:
//! the user stops or quits (we do it on the way out), and the process dying without a say (a
//! crash, End task). For that one a second copy of our exe sits and waits for the first to exit:
//! `--watchdog <pid> <state file>`. It shares nothing with the app but the state file.

use freeseries::default_output::{hand_back, take_back, DefaultOutput, Role};
use std::path::PathBuf;

pub const WATCHDOG: &str = "--watchdog";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    /// The output the engine is playing on right now: where the default goes if we vanish.
    pub fallback: Option<String>,
    /// The roles we moved away from our device, and where to, so they can be taken back.
    pub moved: Vec<Role>,
    pub moved_to: Option<String>,
}

pub trait Store {
    fn load(&self) -> Saved;
    fn save(&self, saved: &Saved);
}

pub struct FileStore(pub PathBuf);

impl Store for FileStore {
    fn load(&self) -> Saved {
        std::fs::read_to_string(&self.0).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }
    fn save(&self, saved: &Saved) {
        if let Err(e) = std::fs::write(&self.0, serde_json::to_string(saved).unwrap_or_default()) {
            log::warn!("couldn't remember where the Windows default belongs: {e}");
        }
    }
}

pub struct Handback<D: DefaultOutput, S: Store> {
    pub defaults: D,
    pub store: S,
}

impl<D: DefaultOutput, S: Store> Handback<D, S> {
    /// The engine plays on `output`. `main` is the virtual device Windows should play into.
    pub fn running(&self, output: &str, main: Option<&str>) {
        let mut saved = self.store.load();
        if let (Some(main), Some(moved_to)) = (main, saved.moved_to.take()) {
            let back = take_back(&self.defaults, &std::mem::take(&mut saved.moved), &moved_to, main);
            if !back.is_empty() {
                log::info!("Windows plays into FreeSeries again ({back:?})");
            }
        }
        saved.fallback = Some(output.to_string());
        self.store.save(&saved);
    }

    /// The engine is gone or going. `ours`: every virtual device of ours.
    pub fn away(&self, ours: &[String]) {
        let mut saved = self.store.load();
        let Some(fallback) = saved.fallback.clone() else { return };
        let moved = hand_back(&self.defaults, ours, &fallback);
        if !moved.is_empty() {
            log::info!("the engine is away: Windows plays straight to the output again ({moved:?})");
            saved.moved.extend(moved.into_iter().filter(|r| !saved.moved.contains(r)).collect::<Vec<_>>());
            saved.moved_to = Some(fallback);
            self.store.save(&saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Windows(RefCell<HashMap<String, String>>);
    impl DefaultOutput for &Windows {
        fn get(&self, role: Role) -> Option<String> {
            self.0.borrow().get(&format!("{role:?}")).cloned()
        }
        fn set(&self, role: Role, device: &str) -> Result<(), String> {
            self.0.borrow_mut().insert(format!("{role:?}"), device.into());
            Ok(())
        }
    }
    #[derive(Default)]
    struct Memory(RefCell<Saved>);
    impl Store for &Memory {
        fn load(&self) -> Saved {
            self.0.borrow().clone()
        }
        fn save(&self, saved: &Saved) {
            *self.0.borrow_mut() = saved.clone();
        }
    }

    #[test]
    fn the_default_follows_the_engine_even_across_a_crash() {
        let windows = Windows(RefCell::new(HashMap::from([("Console".into(), "virtual".into()), ("Multimedia".into(), "virtual".into()), ("Communications".into(), "virtual".into())])));
        let memory = Memory::default();
        let ours = ["virtual".to_string()];
        let app = Handback { defaults: &windows, store: &memory };

        app.away(&ours);
        assert_eq!((&windows).get(Role::Console).as_deref(), Some("virtual"), "never played: there is nowhere to hand back to");

        app.running("headset", Some("virtual"));
        // The app dies here. The watchdog is a different process: all it has is the store.
        let watchdog = Handback { defaults: &windows, store: &memory };
        watchdog.away(&ours);
        assert_eq!((&windows).get(Role::Console).as_deref(), Some("headset"));
        assert_eq!((&windows).get(Role::Communications).as_deref(), Some("headset"));

        // Next start, and the headset is off this time: the engine comes up on the monitor.
        let again = Handback { defaults: &windows, store: &memory };
        again.running("monitor", Some("virtual"));
        assert_eq!((&windows).get(Role::Console).as_deref(), Some("virtual"));
        assert_eq!(memory.0.borrow().clone(), Saved { fallback: Some("monitor".into()), moved: vec![], moved_to: None });
    }
}
