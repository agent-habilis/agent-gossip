pub(crate) use self::a2a_alpn::A2A_ALPN;
pub(crate) use self::a2a_ticket_label::A2A_TICKET_LABEL;
use self::announce::announce;
pub(crate) use self::bridge::connect;
pub(crate) use self::expose_params::{ExposeParams, expose};
pub(crate) use self::secret_len::SECRET_LEN;
pub(crate) use self::ticket_directory::{TicketDirectory, TicketDirectoryEvent};
pub(crate) use self::ticket_requires_password::ticket_requires_password;
use self::wait_online::wait_online;

mod a2a_alpn;
mod a2a_ticket;
mod a2a_ticket_label;
mod announce;
#[expect(
    clippy::module_inception,
    reason = "the primary type Bridge is named after this directory, per O9"
)]
mod bridge;
mod card_rewriter;
mod expose_params;
mod local_gate;
mod secret_len;
mod ticket_directory;
mod ticket_requires_password;
mod wait_online;

#[cfg(test)]
mod tests;
