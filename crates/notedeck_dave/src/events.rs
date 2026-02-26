use enostr::{PoolEventBuf, PoolRelay, RelayEvent, RelayMessage, RelayPool};
use notedeck::{AppContext, UnknownIds};
use tracing::{error, info};

pub fn try_process_events_core(
    app_ctx: &mut AppContext<'_>,
    pool: &mut enostr::RelayPool,
    ctx: &egui::Context,
    mut receive: impl FnMut(&mut AppContext, &mut RelayPool, PoolEventBuf),
) {
    let ctx2 = ctx.clone();
    let wakeup = move || {
        ctx2.request_repaint();
    };

    pool.keepalive_ping(wakeup);

    // NOTE: we don't use the while let loop due to borrow issues
    #[allow(clippy::while_let_loop)]
    loop {
        let ev = if let Some(ev) = pool.try_recv() {
            ev.into_owned()
        } else {
            break;
        };

        match (&ev.event).into() {
            RelayEvent::Opened => {
                tracing::trace!("Opened relay {}", ev.relay);
            }
            RelayEvent::Closed => tracing::warn!("{} connection closed", &ev.relay),
            RelayEvent::Other(msg) => {
                tracing::trace!("relay {} sent other event {:?}", ev.relay, &msg)
            }
            RelayEvent::Error(error) => error!("relay {} had error: {error:?}", &ev.relay),
            RelayEvent::Message(msg) => {
                process_message_core(app_ctx, pool, &ev.relay, &msg);
            }
        }

        receive(app_ctx, pool, ev);
    }

    if app_ctx.unknown_ids.ready_to_send() {
        pool_unknown_id_send(app_ctx.unknown_ids, pool);
    }
}

fn process_message_core(
    ctx: &mut AppContext<'_>,
    pool: &mut enostr::RelayPool,
    relay: &str,
    msg: &RelayMessage,
) {
    match msg {
        RelayMessage::Event(_subid, ev) => {
            let relay = if let Some(relay) = pool.relays.iter().find(|r| r.url() == relay) {
                relay
            } else {
                error!("couldn't find relay {} for note processing!?", relay);
                return;
            };

            match relay {
                PoolRelay::Websocket(_) => {
                    //info!("processing event {}", event);
                    tracing::trace!("processing event {ev}");
                    if let Err(err) = ctx.ndb.process_event_with(
                        ev,
                        nostrdb::IngestMetadata::new()
                            .client(false)
                            .relay(relay.url()),
                    ) {
                        error!("error processing event {ev}: {err}");
                    }
                }
                PoolRelay::Multicast(_) => {
                    // multicast events are client events
                    if let Err(err) = ctx.ndb.process_event_with(
                        ev,
                        nostrdb::IngestMetadata::new()
                            .client(true)
                            .relay(relay.url()),
                    ) {
                        error!("error processing multicast event {ev}: {err}");
                    }
                }
            }
        }
        RelayMessage::Notice(msg) => tracing::warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(id) => {
            tracing::trace!("Relay {} received eose: {id}", relay)
        }
        RelayMessage::Closed(sid, reason) => {
            tracing::trace!(
                "Relay {} with sub {sid} received close because: {reason}",
                relay
            );
        }
    }
}

fn pool_unknown_id_send(unknown_ids: &mut UnknownIds, pool: &mut enostr::RelayPool) {
    tracing::debug!("unknown_id_send called on: {:?}", &unknown_ids);
    let filter = unknown_ids.filter().expect("filter");
    tracing::debug!(
        "Getting {} unknown ids from relays",
        unknown_ids.ids_iter().len()
    );
    let msg = enostr::ClientMessage::req("unknownids".to_string(), filter);
    unknown_ids.clear();
    pool.send(&msg);
}
