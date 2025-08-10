use crate::event::Invoice;
use lnsocket::CallOpts;
use lnsocket::CommandoClient;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
struct UpdatedInvoicesResponse {
    updated: u64,
}

#[derive(Deserialize)]
struct PayIndexInvoices {
    invoices: Vec<PayIndexScan>,
}

#[derive(Deserialize)]
struct PayIndexScan {
    pay_index: Option<u64>,
}

async fn find_lastpay_index(commando: Arc<CommandoClient>) -> Result<Option<u64>, lnsocket::Error> {
    const PAGE: u64 = 250;
    // 1) get the current updated tail
    let created_value = commando
        .call(
            "wait",
            json!({"subsystem":"invoices","indexname":"updated","nextvalue":0}),
        )
        .await?;
    let response: UpdatedInvoicesResponse =
        serde_json::from_value(created_value).map_err(|_| lnsocket::Error::Json)?;

    // start our window at the tail
    let mut start_at = response
        .updated
        .saturating_add(1) // +1 because we want max(1, updated - PAGE + 1)
        .saturating_sub(PAGE)
        .max(1);

    loop {
        // 2) fetch a window (indexed by "updated")
        let val = commando
            .call_with_opts(
                "listinvoices",
                json!({
                    "index": "updated",
                    "start": start_at,
                    "limit": PAGE,
                }),
                // only fetch the one field we care about
                CallOpts::default().filter(json!({
                    "invoices": [{"pay_index": true}]
                })),
            )
            .await?;

        let parsed: PayIndexInvoices =
            serde_json::from_value(val).map_err(|_| lnsocket::Error::Json)?;

        if let Some(pi) = parsed.invoices.iter().filter_map(|inv| inv.pay_index).max() {
            return Ok(Some(pi));
        }

        // 4) no paid invoice in this slice—step back or bail
        if start_at == 1 {
            return Ok(None);
        }

        start_at = start_at.saturating_sub(PAGE).max(1);
    }
}

pub async fn fetch_paid_invoices(
    commando: Arc<CommandoClient>,
    limit: u32,
) -> Result<Vec<Invoice>, lnsocket::Error> {
    use tokio::task::JoinSet;

    // look for an invoice with the last paid index
    let Some(lastpay_index) = find_lastpay_index(commando.clone()).await? else {
        // no paid invoices
        return Ok(vec![]);
    };

    let mut set: JoinSet<Result<Invoice, lnsocket::Error>> = JoinSet::new();
    let start = lastpay_index.saturating_sub(limit as u64);

    // 3) Fire off at most `concurrency` `waitanyinvoice` calls at a time,
    //    collect all successful responses into a Vec.
    // fire them ALL at once
    for idx in start..lastpay_index {
        let c = commando.clone();
        set.spawn(async move {
            let val = c
                .call(
                    "waitanyinvoice",
                    serde_json::json!({ "lastpay_index": idx }),
                )
                .await?;
            let parsed: Invoice = serde_json::from_value(val).map_err(|_| lnsocket::Error::Json)?;
            Ok(parsed)
        });
    }

    let mut results = Vec::with_capacity(limit as usize);
    while let Some(res) = set.join_next().await {
        results.push(res.map_err(|_| lnsocket::Error::Io(std::io::ErrorKind::Interrupted))??);
    }

    results.sort_by(|a, b| a.updated_index.cmp(&b.updated_index));

    Ok(results)
}

// wip watch subsystem
/*
async fn watch_subsystem(
    commando: CommandoClient,
    subsystem: WaitSubsystem,
    index: WaitIndex,
    event_tx: UnboundedSender<Event>,
    mut cancel_rx: Receiver<()>,
) {
    // Step 1: Fetch current index value so we can back up ~20
    let mut nextvalue: u64 = match commando
        .call(
            "wait",
            serde_json::json!({
                "indexname": index.as_str(),
                "subsystem": subsystem.as_str(),
                "nextvalue": 0
            }),
        )
        .await
    {
        Ok(v) => {
            // You showed the result has `updated` as the current highest index
            let current = v.get("updated").and_then(|x| x.as_u64()).unwrap_or(0);
            current.saturating_sub(20) // back up 20, clamp at 0
        }
        Err(err) => {
            tracing::warn!("initial wait(…nextvalue=0) failed: {}", err);
            0
        }
    };

    loop {
        // You can add a timeout to avoid hanging forever in weird network states.
        let fut = commando.call(
            "wait",
            serde_json::to_value(WaitRequest {
                indexname: "invoices".into(),
                subsystem: "lightningd".into(),
                nextvalue,
            })
            .unwrap(),
        );

        tokio::select! {
            _ = &mut cancel_rx => {
                // graceful shutdown
                break;
            }

            res = fut => {
                match res {
                    Ok(v) => {
                        // Typical shape: { "nextvalue": n, "invoicestatus": { ... } } (varies by plugin/index)
                        // Adjust these lookups for your node’s actual wait payload.
                        if let Some(nv) = v.get("nextvalue").and_then(|x| x.as_u64()) {
                            nextvalue = nv + 1;
                        } else {
                            // Defensive: never get stuck — bump at least by 1
                            nextvalue += 1;
                        }

                        // Inspect/route
                        let kind = v.get("status").and_then(|s| s.as_str());
                        let ev = match kind {
                            Some("paid") => ClnResponse::Invoice(InvoiceEvent::Paid(v.clone())),
                            Some("created") => ClnResponse::Invoice(InvoiceEvent::Created(v.clone())),
                            _ => ClnResponse::Invoice(InvoiceEvent::Other(v.clone())),
                        };
                        let _ = event_tx.send(Event::Response(ev));
                    }
                    Err(err) => {
                        tracing::warn!("wait(invoices) error: {err}");
                        // small backoff so we don't tight-loop on persistent errors
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        }
    }
}
*/
