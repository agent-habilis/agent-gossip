use super::ticket::A2aTicket;

pub(crate) fn ticket_requires_password(ticket: &str) -> bool {
    A2aTicket::decode(ticket).is_ok_and(|decoded| decoded.password)
}
