# ⚡ clndash

Your Core Lightning dashboard, **without the server nonsense**.

clndash is a weird little experiment: a [notedeck][notedeck] app that talks to your node **directly over the Lightning Network** using [lnsocket][lnsocket] + [Commando][commando] RPCs.

No HTTP. No nginx. No VPS.
Just open clndash, point it at your node, and boom — you’re in.

<img src="https://jb55.com/s/476285c50d06c3ce.png" width="50%" />

---

## 🤯 Why?

Because sometimes you just want to *see your channels* and *check invoices* without SSH-ing into a box and typing `lightning-cli`.

And because LN is already a secure, encrypted connection layer — why not just use that?

---

## 🔥 Features (as of today)

* **Plug-and-play LN connection** – powered by [lnsocket][lnsocket]
* **Commando RPC** – all dashboard data is fetched directly from your CLN node over Lightning
* **Channel overview** – total capacity, inbound/outbound liquidity, largest channel, and pretty bars
* **Invoices** – shows recent paid invoices (with zap previews if they came from Nostr)
* **No extra daemons** – you don’t need to run a server to use it

---

## 🪄 Nostr Bonus

Because it’s a notedeck app, clndash can **render zaps** inline.
Yes, your Core Lightning dashboard can now show you when someone on Nostr just sent you sats and why.

---

## 🏗 Still Baking

This is WIP.
You’ll probably hit bugs. UI might be janky. Some features may vanish or suddenly mutate.

If you’re reading this and still excited — you’re the exact audience.

---

## 🛠 How to connect

1. Get your node’s **public address** (host\:port) and a **Commando rune** with safe permissions.
2. Set them as environment variables:

   ```bash
   export CLNDASH_HOST="node.example.com:9735"
   export CLNDASH_RUNE="your_rune_here"
   ```
3. Run clndash inside notedeck.
4. Bask in the glow of real-time LN data over an LN connection.

---

## ⚠️ Disclaimer

* Don’t give it a rune that can spend your funds.
* Don’t blame me if you break something — this is experimental territory.
* If it connects on the first try, buy yourself a beer.

---

If you like living on the edge of LN/Nostr tooling, you’ll like this.
If you don’t… you’ll probably want to wait a bit.


[commando]: https://docs.corelightning.org/reference/commando
[lnsocket]: https://github.com/jb55/lnsocket-rs
[notedeck]: https://github.com/damus-io/notedeck
