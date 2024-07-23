use crate::{route::Route, thread::Thread, Damus};
use enostr::NoteId;
use nostrdb::Transaction;
use tracing::{info, warn};

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BarAction {
    Reply,
    OpenThread,
}

impl BarAction {
    pub fn execute(
        self,
        app: &mut Damus,
        timeline: usize,
        replying_to: &[u8; 32],
        txn: &Transaction,
    ) {
        match self {
            BarAction::Reply => {
                let timeline = &mut app.timelines[timeline];
                timeline
                    .routes
                    .push(Route::Reply(NoteId::new(replying_to.to_owned())));
                timeline.navigating = true;
            }

            BarAction::OpenThread => {
                {
                    let timeline = &mut app.timelines[timeline];
                    timeline
                        .routes
                        .push(Route::Thread(NoteId::new(replying_to.to_owned())));
                    timeline.navigating = true;
                }

                let root_id = crate::note::root_note_id_from_selected_id(app, txn, replying_to);
                let thread = app.threads.thread_mut(&app.ndb, txn, root_id);

                // only start a subscription on nav and if we don't have
                // an active subscription for this thread.
                if thread.subscription().is_none() {
                    *thread.subscription_mut() = app.ndb.subscribe(Thread::filters(root_id)).ok();

                    match thread.subscription() {
                        Some(_sub) => {
                            thread.subscribers += 1;
                            info!(
                                "Locally subscribing to thread. {} total active subscriptions, {} on this thread",
                                app.ndb.subscription_count(),
                                thread.subscribers,
                            );
                        }
                        None => warn!(
                            "Error subscribing locally to selected note '{}''s thread",
                            hex::encode(replying_to)
                        ),
                    }
                } else {
                    thread.subscribers += 1;
                    info!(
                        "Re-using existing thread subscription. {} total active subscriptions, {} on this thread",
                        app.ndb.subscription_count(),
                        thread.subscribers,
                    )
                }
            }
        }
    }
}
