use std::{cell::Cell, collections::HashSet, rc::Rc};

use nsh_core::prelude::*;

#[derive(Clone)]
pub(crate) struct ActionOwner {
    owner: Owner,
    open_popups: Signal<HashSet<u64>>,
    next_popup_id: Rc<Cell<u64>>,
}

impl ActionOwner {
    pub(crate) fn install() {
        // Capture FalconApp's root before transient popup and Select owners are created.
        provide_context(Self {
            owner: Owner::current(),
            open_popups: signal(HashSet::new()),
            next_popup_id: Rc::new(Cell::new(0)),
        });
    }

    pub(crate) fn current() -> Self {
        use_context::<Self>().expect("ActionOwner must be installed by FalconApp")
    }

    pub(crate) fn run<T>(&self, callback: impl FnOnce() -> T) -> T {
        self.owner.clone().run(callback)
    }

    pub(crate) fn open_popups(&self) -> Signal<HashSet<u64>> {
        self.open_popups
    }

    pub(crate) fn popup_token(&self) -> PopupToken {
        let id = self.next_popup_id.get();
        self.next_popup_id.set(id.wrapping_add(1));
        let token = PopupToken {
            id,
            open_popups: self.open_popups,
        };
        let cleanup_token = token.clone();
        cleanup(move || cleanup_token.set_open(false));
        token
    }
}

#[derive(Clone)]
pub(crate) struct PopupToken {
    id: u64,
    open_popups: Signal<HashSet<u64>>,
}

impl PopupToken {
    pub(crate) fn set_open(&self, open: bool) {
        self.open_popups.update(|open_popups| {
            if open {
                open_popups.insert(self.id);
            } else {
                open_popups.remove(&self.id);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_core::element::reactive::{Runtime, create_root};

    #[test]
    fn popup_tokens_remain_independent_and_cleanup_on_owner_disposal() {
        let runtime = Runtime::default();
        let _entered = runtime.enter();
        let (root, (open_popups, first, second, popup_owner, disposed_popup)) = create_root(|| {
            ActionOwner::install();
            let action_owner = ActionOwner::current();
            let open_popups = action_owner.open_popups();
            let first = action_owner.popup_token();
            let second = action_owner.popup_token();
            let (popup_owner, disposed_popup) = create_root(|| {
                let popup = action_owner.popup_token();
                popup.set_open(true);
                popup
            });
            (open_popups, first, second, popup_owner, disposed_popup)
        });

        first.set_open(true);
        second.set_open(true);
        assert_eq!(open_popups.get().len(), 3);
        first.set_open(false);
        assert_eq!(open_popups.get().len(), 2);

        second.set_open(false);
        assert_eq!(open_popups.get().len(), 1);
        assert!(!open_popups.get().is_empty());

        popup_owner.dispose();
        assert!(open_popups.get().is_empty());
        drop(disposed_popup);
        root.dispose();
    }
}
