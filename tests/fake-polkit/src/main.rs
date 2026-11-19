//! A stand-in for polkitd on the private test bus.
//!
//! Every `CheckAuthorization` reads `FAKE_POLKIT_CONTROL`:
//!
//! ```text
//! allow
//! delay 3
//! ```
//!
//! The first line is `allow` or `deny`, the optional second line makes the
//! answer take that many seconds. A cancellation ends the wait at once, the
//! way the real daemon ends a check.
//!
//! Each check and cancellation is printed, one per line, for the scripts to
//! assert on.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::Connection;
use zbus::interface;
use zbus::zvariant::OwnedValue;

struct Fake {
    cancelled: Arc<Mutex<HashSet<String>>>,
}

fn control() -> (bool, u64) {
    let Some(path) = env::var_os("FAKE_POLKIT_CONTROL") else {
        return (true, 0);
    };
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut lines = text.lines();
    let allow = lines.next().map(|l| l.trim() != "deny").unwrap_or(true);
    let delay = lines
        .next()
        .and_then(|l| l.trim().strip_prefix("delay "))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    (allow, delay)
}

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Fake {
    async fn check_authorization(
        &self,
        subject: (String, HashMap<String, OwnedValue>),
        action_id: String,
        details: HashMap<String, String>,
        _flags: u32,
        cancellation_id: String,
    ) -> zbus::fdo::Result<(bool, bool, HashMap<String, String>)> {
        let (allow, delay) = control();
        println!(
            "check subject={} action={action_id} app={} id={cancellation_id}",
            subject.0,
            details.get("app_id").map(String::as_str).unwrap_or(""),
        );
        let deadline = Instant::now() + Duration::from_secs(delay);
        while Instant::now() < deadline {
            if self.cancelled.lock().unwrap().contains(&cancellation_id) {
                println!("cancelled-before-reply {cancellation_id}");
                return Ok((false, false, HashMap::new()));
            }
            async_io::Timer::after(Duration::from_millis(25)).await;
        }
        Ok((allow, false, HashMap::new()))
    }

    async fn cancel_check_authorization(&self, cancellation_id: String) -> zbus::fdo::Result<()> {
        println!("cancel {cancellation_id}");
        self.cancelled.lock().unwrap().insert(cancellation_id);
        Ok(())
    }
}

fn main() {
    async_io::block_on(async {
        let conn = Connection::system().await.expect("system bus");
        conn.request_name("org.freedesktop.PolicyKit1")
            .await
            .expect("claim the polkit name");
        conn.object_server()
            .at(
                "/org/freedesktop/PolicyKit1/Authority",
                Fake { cancelled: Arc::default() },
            )
            .await
            .expect("serve the authority");
        println!("fake polkit ready");
        std::future::pending::<()>().await;
    });
}
