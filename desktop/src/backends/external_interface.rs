use ruffle_core::context::UpdateContext;
use ruffle_core::external::{ExternalInterfaceProvider, Value as ExternalValue};
use url::Url;

pub struct DesktopExternalInterfaceProvider {
    pub spoof_url: Option<Url>,
}

fn is_location_href(code: &str) -> bool {
    matches!(
        code,
        "document.location.href" | "window.location.href" | "top.location.href"
    )
}

impl ExternalInterfaceProvider for DesktopExternalInterfaceProvider {
    fn call_method(
        &self,
        _context: &mut UpdateContext<'_>,
        name: &str,
        args: &[ExternalValue],
    ) -> ExternalValue {
        if let Some(ref url) = self.spoof_url {
            // Check for e.g. "window.location.href.toString"
            if let Some(name) = name.strip_suffix(".toString") {
                if is_location_href(name) {
                    return url.to_string().into();
                }
            }
        }

        if name == "eval" {
            if let Some(ref url) = self.spoof_url {
                if let [ExternalValue::String(ref code)] = args {
                    if is_location_href(code) {
                        return ExternalValue::String(url.to_string());
                    }
                }
            }

            tracing::warn!("Trying to call eval with ExternalInterface: {args:?}");
            return ExternalValue::Undefined;
        }

        if name == "console.log" {
            let mut log = String::new();
            for arg in args {
                match arg {
                    ExternalValue::String(s) => log.push_str(s),
                    ExternalValue::Number(n) => log.push_str(&n.to_string()),
                    ExternalValue::Bool(b) => log.push_str(&b.to_string()),
                    ExternalValue::Undefined => log.push_str("undefined"),
                    ExternalValue::Null => log.push_str("null"),
                    _ => log.push_str("<unknown>"),
                }
                log.push(' ');
            }
            log.pop(); // remove last space

            tracing::info!("ExternalInterface: console.log: {log}");
            return ExternalValue::Undefined;
        }

        if name == "window.navigator.userAgent.toString" {
            return ExternalValue::String("mundo-gaturro-desktop".to_string());
        }

        tracing::warn!("Trying to call unknown ExternalInterface method: {name}");
        ExternalValue::Undefined
    }

    fn on_callback_available(&self, _name: &str) {}

    fn get_id(&self) -> Option<String> {
        None
    }
}
