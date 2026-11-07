//! The one place adapter code reads a `MUNUNU_*` variable that a test may want to control
//! (mununu#880).
//!
//! In the product [`read`] is `std::env::var`. Under `cfg(test)` a thread-local override set by
//! [`override_for_test`] or [`unset_for_test`] wins, so a test can point a locator at a bogus
//! path, or a diagnostic at a temp file, without writing the process environment. The
//! environment is process-global and libtest runs a binary's tests in parallel: a test that set
//! `MUNUNU_MAX_PROCESS_MEMORY_BYTES` to 1 GiB made an unrelated verify-auto row abstain
//! (mununu#871), and every tool-path write was a window in which a concurrent test saw no tool.
//!
//! The override is per thread, so it is seen by code the test calls on its own thread: every
//! locator, the breadcrumb's `from_env`, the shadow dump. A read on a thread the callee spawns
//! sees the real environment; none of the overridden reads happen there today.

/// The value of `name`: the current thread's test override when one is in force, else the
/// process environment. `None` is "unset" in both cases.
pub(crate) fn read(name: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(v) = overrides::lookup(name) {
        return v;
    }
    std::env::var(name).ok()
}

#[cfg(test)]
mod overrides {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// One live override: its name, the guard's id (so a guard removes exactly its own entry,
    /// whatever order the guards drop in), and the value (`None` = overridden to unset).
    type Entry = (&'static str, u64, Option<String>);

    thread_local! {
        /// A stack per thread: the newest override of a name wins.
        static STACK: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    }
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    /// `Some(value)` when an override for `name` is in force on this thread; the inner `None`
    /// means "overridden to unset".
    pub(super) fn lookup(name: &str) -> Option<Option<String>> {
        STACK.with(|s| {
            s.borrow()
                .iter()
                .rev()
                .find(|(n, _, _)| *n == name)
                .map(|(_, _, v)| v.clone())
        })
    }

    pub(super) fn push(name: &'static str, value: Option<String>) -> u64 {
        let id = NEXT_ID.fetch_add(1, Relaxed);
        STACK.with(|s| s.borrow_mut().push((name, id, value)));
        id
    }

    pub(super) fn pop(id: u64) {
        STACK.with(|s| s.borrow_mut().retain(|(_, i, _)| *i != id));
    }
}

/// Holds an override in force; dropping it removes that override and nothing else.
#[cfg(test)]
#[must_use = "the override lasts only as long as the guard"]
pub(crate) struct EnvOverride {
    id: u64,
}

#[cfg(test)]
impl Drop for EnvOverride {
    fn drop(&mut self) {
        overrides::pop(self.id);
    }
}

/// Make [`read`] return `value` for `name` on this thread until the guard drops.
#[cfg(test)]
pub(crate) fn override_for_test(
    name: &'static str,
    value: impl AsRef<std::ffi::OsStr>,
) -> EnvOverride {
    let id = overrides::push(name, Some(value.as_ref().to_string_lossy().into_owned()));
    EnvOverride { id }
}

/// Make [`read`] return `None` for `name` on this thread until the guard drops, whatever the
/// process environment holds.
#[cfg(test)]
pub(crate) fn unset_for_test(name: &'static str) -> EnvOverride {
    let id = overrides::push(name, None);
    EnvOverride { id }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "MUNUNU_ENV_VAR_TEST_880";

    #[test]
    fn an_override_is_seen_on_its_thread_and_gone_when_its_guard_drops() {
        assert_eq!(read(NAME), std::env::var(NAME).ok());
        {
            let _g = override_for_test(NAME, "one");
            assert_eq!(read(NAME).as_deref(), Some("one"));
            let _h = unset_for_test(NAME);
            assert_eq!(read(NAME), None, "the newest override wins");
        }
        assert_eq!(read(NAME), std::env::var(NAME).ok());
    }

    #[test]
    fn an_override_is_invisible_to_another_thread() {
        let _g = override_for_test(NAME, "mine");
        let seen = std::thread::spawn(|| read(NAME)).join().unwrap();
        assert_eq!(seen, std::env::var(NAME).ok());
    }

    #[test]
    fn guards_dropped_out_of_order_leave_the_right_override() {
        let a = override_for_test(NAME, "a");
        let b = override_for_test(NAME, "b");
        drop(a);
        assert_eq!(read(NAME).as_deref(), Some("b"));
        drop(b);
        assert_eq!(read(NAME), std::env::var(NAME).ok());
    }
}
