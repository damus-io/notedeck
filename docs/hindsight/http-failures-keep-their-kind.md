# HTTP failures keep their kind

ehttp flattens every ureq transport error to a String, so telling refused from a
bad URL means sniffing text; a fetch returning ureq's ErrorKind would type it.

- headway:cashflow/admit-slide-trophy — read ehttp 0.5 and ureq 2.12 to learn the error text, then mapped every `Err` to `RpcError::Unreachable` except a `Failed to read response body` prefix; a bad URL or TLS failure falls back like a refused connection
