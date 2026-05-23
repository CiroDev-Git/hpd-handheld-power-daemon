//! Polkit authorization helper.
//!
//! The interface methods on [`crate::service::PowerDaemonInterface`] call
//! [`check`] before enqueuing a privileged `Transition`. We talk to
//! polkit by hand over `org.freedesktop.PolicyKit1.Authority` instead of
//! pulling in a dedicated crate — the call is small enough that the
//! extra dependency is not worth it.
//!
//! ## Failure mode: fail-closed
//!
//! Any error from polkit (proxy creation failure, method call timeout,
//! malformed reply) is logged as a warning and the call returns `false`.
//! The daemon would rather refuse a legitimate request than allow an
//! unauthenticated one through.
//!
//! ## Simulator builds
//!
//! Under `#[cfg(feature = "simulator")]` the helper unconditionally
//! returns `true`. Session-bus runs on macOS / dev hosts have no polkit
//! authority to talk to and gating every setter would make the
//! simulator unusable.

use tracing::debug;
#[cfg(not(feature = "simulator"))]
use tracing::warn;

use crate::actions::PolkitAction;

/// Polkit-managed authorization check for a single privileged action.
///
/// `header` must be the incoming method-call header; we read the
/// sender's unique bus name from it to build the polkit subject.
/// Returns `true` only when polkit explicitly says "authorized".
pub async fn check(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    action: PolkitAction,
) -> bool {
    check_inner(conn, header, action.as_id()).await
}

#[cfg(feature = "simulator")]
async fn check_inner(
    _conn: &zbus::Connection,
    _header: &zbus::message::Header<'_>,
    action_id: &str,
) -> bool {
    debug!(action = action_id, "Polkit bypassed (simulator build)");
    true
}

#[cfg(not(feature = "simulator"))]
async fn check_inner(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    action_id: &str,
) -> bool {
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    let Some(sender) = header.sender() else {
        warn!(action = action_id, "Method call has no sender; denying (fail-closed)");
        return false;
    };
    let sender_name = sender.as_str().to_string();

    // polkit subject for a D-Bus caller: ("system-bus-name", {"name": "<unique-name>"}).
    let mut subject_details: HashMap<String, Value<'_>> = HashMap::new();
    subject_details.insert("name".to_string(), Value::new(sender_name.clone()));
    let subject = ("system-bus-name".to_string(), subject_details);

    let details: HashMap<&str, &str> = HashMap::new();
    // 1 = AllowUserInteraction: let polkit pop a prompt agent if needed.
    let flags: u32 = 1;
    let cancellation_id = "";

    let proxy = match zbus::Proxy::new(
        conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, action = action_id, "Polkit proxy creation failed; denying (fail-closed)");
            return false;
        }
    };

    let result: zbus::Result<(bool, bool, HashMap<String, String>)> = proxy
        .call(
            "CheckAuthorization",
            &(subject, action_id, details, flags, cancellation_id),
        )
        .await;

    match result {
        Ok((is_authorized, is_challenge, _)) => {
            debug!(
                action = action_id,
                sender = %sender_name,
                authorized = is_authorized,
                challenge = is_challenge,
                "Polkit check returned"
            );
            is_authorized
        }
        Err(e) => {
            warn!(error = %e, action = action_id, sender = %sender_name, "Polkit call failed; denying (fail-closed)");
            false
        }
    }
}
