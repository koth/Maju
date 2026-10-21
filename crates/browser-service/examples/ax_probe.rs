//! Measure the accessibility capture against a live page.
//!
//! This is the harness behind the numbers in
//! `docs/browser-use-replication.md`: it drives the real observe → act → observe
//! loop against a browser over CDP and prints how many characters each step
//! costs the model. It is a measuring tool, not a feature — nothing in the
//! product depends on it.
//!
//! ```text
//! cargo run -p browser-service --example ax_probe -- http://127.0.0.1:9333 --text
//! ```
//!
//! The browser must already be running with `--remote-debugging-port` and have
//! a page open; the example never launches one.

use browser_service::ax::{
    Action, DiffOptions, ObserveMode, Observed, Observer, RenderOptions, SettleOptions, Target,
    capture,
};
use browser_service::cdp::CdpClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let endpoint = args.next().unwrap_or_else(|| "http://127.0.0.1:9333".to_string());
    let show_text = args.any(|arg| arg == "--text");

    let client = CdpClient::connect(&endpoint).await?;
    let mut observer = Observer::new();

    // 1. The first look at the page.
    let first = capture(&client, None, SettleOptions::default()).await?;
    let target = first.target_id.clone();
    observer.begin_capture(&target);
    let observed = observer.observe(
        &first.doc,
        ObserveMode::Full,
        RenderOptions::default(),
        DiffOptions::default(),
    );
    report("observe #1 (full)", &observed, show_text);

    // 2. Fill three fields, observing after each one. This is the loop the
    //    model actually runs, and the diff is what it costs.
    let steps: [(&str, &str, &str); 3] = [
        ("text field", "Customer", "Acme Ltd"),
        ("text field", "Billing email", "billing@acme.example"),
        ("spinbutton", "Amount", "1240.00"),
    ];
    for (role, label, value) in steps {
        let Some(index) = find_control(&observer, role, label) else {
            eprintln!("no {role} named {label:?}");
            continue;
        };
        let index = index as usize;
        let backend = observer.element(index)?;
        browser_service::ax::act(
            &client,
            &target,
            Target::Element { index, backend_id: backend },
            &Action::Fill(value.to_string()),
        )
        .await?;

        // Verify against the page itself: a capture can only tell us what the
        // accessibility tree chose to expose.
        let check = browser_service::ax::evaluate_on_page(
            &client,
            &target,
            "(() => ({ active: document.activeElement && document.activeElement.id, \
              customer: document.getElementById('customer').value, \
              email: document.getElementById('email').value, \
              amount: document.getElementById('amount').value }))()",
        )
        .await?;
        println!("  page says: {check}");

        let next = capture(&client, Some(&target), SettleOptions::default()).await?;
        let observed = observer.observe(
            &next.doc,
            ObserveMode::State,
            RenderOptions::default(),
            DiffOptions::default(),
        );
        report(&format!("fill {label:?} → observe"), &observed, show_text);
    }

    // 3. Submit, then look once more: the status line appearing is the result.
    if let Some(index) = find_control(&observer, "button", "Create invoice") {
        let index = index as usize;
        let backend = observer.element(index)?;
        browser_service::ax::act(
            &client,
            &target,
            Target::Element {
                index,
                backend_id: backend,
            },
            &Action::Click,
        )
        .await?;
        let next = capture(&client, Some(&target), SettleOptions::default()).await?;
        let observed = observer.observe(
            &next.doc,
            ObserveMode::State,
            RenderOptions::default(),
            DiffOptions::default(),
        );
        report("click submit → observe", &observed, show_text);
    }

    // 3. Click the submit button by *point* rather than by number, to prove the
    //    fallback path lands on the same control. The status line is cleared
    //    first so its reappearance is the evidence.
    let point = browser_service::ax::evaluate_on_page(
        &client,
        &target,
        "(() => { document.getElementById('status').textContent = ''; \
          const r = document.getElementById('submit').getBoundingClientRect(); \
          return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()",
    )
    .await?;
    if let (Some(x), Some(y)) = (
        point.get("x").and_then(|value| value.as_f64()),
        point.get("y").and_then(|value| value.as_f64()),
    ) {
        browser_service::ax::act(
            &client,
            &target,
            Target::Point { x, y },
            &Action::Click,
        )
        .await?;
        let next = capture(&client, Some(&target), SettleOptions::default()).await?;
        let observed = observer.observe(
            &next.doc,
            ObserveMode::State,
            RenderOptions::default(),
            DiffOptions::default(),
        );
        report("click by point → observe", &observed, show_text);
    }

    // 4. Scroll by wheel at a point: this is the other thing a point target
    //    exists for. The scroll offset is the evidence — the accessibility tree
    //    lists offscreen content either way.
    let before_scroll = browser_service::ax::evaluate_on_page(&client, &target, "window.scrollY")
        .await?
        .as_f64()
        .unwrap_or_default();
    browser_service::ax::act(
        &client,
        &target,
        Target::Point { x: 400.0, y: 300.0 },
        &Action::Scroll { dx: 0.0, dy: -200.0 },
    )
    .await?;
    let after_scroll = browser_service::ax::evaluate_on_page(&client, &target, "window.scrollY")
        .await?
        .as_f64()
        .unwrap_or_default();
    println!("  scrollY: {before_scroll} -> {after_scroll}");
    let next = capture(&client, Some(&target), SettleOptions::default()).await?;
    let observed = observer.observe(
        &next.doc,
        ObserveMode::State,
        RenderOptions::default(),
        DiffOptions::default(),
    );
    report("scroll → observe", &observed, show_text);

    client.close().await;
    Ok(())
}

/// Find a control by role and name, the way a model reads a line.
///
/// Matching on "contains" would find the `<label>` before the field it labels —
/// `text Customer` comes before `text field Customer` — and acting on a label
/// does nothing at all, silently.
fn find_control(observer: &Observer, role: &str, name: &str) -> Option<u64> {
    let wanted = format!("{role} {name}");
    observer
        .lines()
        .iter()
        .find(|line| line.body == wanted || line.body.starts_with(&format!("{wanted},")))
        .map(|line| line.index)
}

fn report(label: &str, observed: &Observed, show_text: bool) {
    let text = observed.text().unwrap_or_default();
    let kind = match observed {
        Observed::Full(_) => "full",
        Observed::Diff(_) => "diff",
        Observed::Unchanged => "unchanged",
    };
    println!(
        "{label}: {kind}, {} chars, {} lines",
        text.chars().count(),
        text.lines().count()
    );
    if show_text {
        println!("---\n{text}---");
    }
}
