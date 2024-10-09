use tracing::debug;

use crate::DamusRef;

pub fn setup_user_relays(damusref: DamusRef) {
    debug!("setup_user_relays starting");
    tokio::spawn(async move {
        do_setup_user_relays(damusref).await;
    });
}

async fn do_setup_user_relays(damusref: DamusRef) {
    debug!("do_setup_user_relays starting");

    let _damus = damusref.lock().await;
    debug!("acquired mut damus");

    debug!("do_setup_user_relays finished");
}
