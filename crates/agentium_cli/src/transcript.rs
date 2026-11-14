//! `agentium log`/`grep`'s message view: the shared transcript filter
//! ([`agentium_core::cli::transcript`]) plus the flags only a terminal run
//! has — `--jsonl`, `--color`, `--pager` and `--follow`.

use agentium_core::cli::transcript::MessageFilter;
use agentium_core::messages::Message;
use cli_term::{ColorWhen, PagerMode};
use nostrdb_net::relay::sync::Result;

pub(crate) use agentium_core::cli::transcript::{first_after, render_message};

/// The transcript-shaping flags for `agentium log`: which messages to show
/// ([`MessageFilter`]), and how to show them. `jsonl` selects the
/// reconstructed-archive path instead (see [`cmd_log`](crate::log::cmd_log));
/// it's carried here so the whole command's shape lives in one value.
pub(crate) struct MessageView {
    /// `--role`/`--last`/`--tools`: which messages to show.
    pub(crate) filter: MessageFilter,
    /// `--jsonl`: emit reconstructed claude-code JSONL instead of the rendered
    /// transcript. Mutually exclusive in effect with the filters above.
    pub(crate) jsonl: bool,
    /// `--color <when>`: whether to ANSI-color the rendered transcript.
    pub(crate) color: ColorWhen,
    /// `--pager`/`--no-pager`: whether to route output through a pager, like
    /// `git log`.
    pub(crate) pager: PagerMode,
    /// `--follow`/`-f`: after printing the current tail, keep streaming each new
    /// message as it lands (`git log`'s transcript, followed like `tail -f`)
    /// until Ctrl-C. A live stream can't be paged or reconstructed as a
    /// point-in-time archive, so it conflicts with `--pager`/`--jsonl` (see
    /// [`MessageView::check_follow`]).
    pub(crate) follow: bool,
}

impl MessageView {
    /// Whether a single message survives the filter (see [`MessageFilter::keep`]).
    pub(crate) fn keep(&self, m: &Message) -> bool {
        self.filter.keep(m)
    }

    /// The messages the filter keeps (see [`MessageFilter::select`]).
    pub(crate) fn select<'a>(&self, messages: &'a [Message]) -> Vec<&'a Message> {
        self.filter.select(messages)
    }

    /// Reject the flag combinations that a live `--follow` can't honor. A live
    /// stream can't be paged (there is no end to page over), and the `--jsonl`
    /// reconstruction is a point-in-time dump of a finished archive, not a
    /// stream — so both conflict with `--follow`. Returns the reason on
    /// conflict; a no-op when `--follow` isn't set.
    pub(crate) fn check_follow(&self) -> Result<()> {
        if !self.follow {
            return Ok(());
        }
        if self.jsonl {
            return Err(
                "cannot --follow --jsonl: the reconstructed source archive is a \
                 point-in-time dump, not a live stream"
                    .into(),
            );
        }
        if self.pager == PagerMode::Always {
            return Err("cannot page a live follow: --follow and --pager conflict".into());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A show-everything view: no role filter, no tail, tools shown, auto
    /// color/pager — the widest view, so a test can narrow one axis at a time.
    pub(crate) fn view_all() -> MessageView {
        MessageView {
            filter: MessageFilter {
                roles: vec![],
                last: None,
                show_tools: true,
            },
            jsonl: false,
            color: ColorWhen::Auto,
            pager: PagerMode::Auto,
            follow: false,
        }
    }

    /// A `MessageView` with only `--follow` and one conflicting flag set,
    /// otherwise default — for exercising [`MessageView::check_follow`].
    fn follow_view(jsonl: bool, pager: PagerMode) -> MessageView {
        MessageView {
            follow: true,
            jsonl,
            pager,
            ..view_all()
        }
    }

    #[test]
    fn check_follow_rejects_paging_and_jsonl() {
        // A plain follow is fine, as is follow with a non-forced pager mode.
        assert!(follow_view(false, PagerMode::Auto).check_follow().is_ok());
        assert!(follow_view(false, PagerMode::Never).check_follow().is_ok());
        // --follow --jsonl and --follow --pager both conflict.
        assert!(follow_view(true, PagerMode::Auto).check_follow().is_err());
        assert!(
            follow_view(false, PagerMode::Always)
                .check_follow()
                .is_err()
        );
        // Without --follow the checks are a no-op even with those flags set.
        let not_following = MessageView {
            follow: false,
            jsonl: true,
            pager: PagerMode::Always,
            ..view_all()
        };
        assert!(not_following.check_follow().is_ok());
    }
}
