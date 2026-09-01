//! The "Server" disclosure on the sign-in and registration forms.
//!
//! A user who has never opened this disclosure sees exactly the form they saw yesterday: identifier
//! and password. A user who has opened it picks the host, port and scheme, and on
//! "Use this server" the disclosure closes and the choice becomes the new form input.
//!
//! The transport is the one choice that is not behind the disclosure: a WebSocket/QUIC pair of
//! selectable labels rides directly under the toggle and one click commits the swap immediately —
//! a transport change never needs the host and port re-confirmed, so it never lives in the draft.
//! QUIC is a real second option, not a placeholder: the choice persists and is validated the same
//! as WebSocket. Connecting over it requires a server with the QUIC listener enabled, and this
//! client's wire path is still WebSocket. The form accepts the choice and never blocks submit, so
//! the user can save a QUIC-capable server and the rest of the surface (REST, captcha) proceeds.
//!
//! The widget writes its accepted endpoint through a callback rather than mutating the caller's
//! state directly. The caller decides whether the new value is accepted into the form state and
//! persisted to settings -- the widget is intentionally ignorant of the persistence path so the
//! same shape can be reused on any screen that wants to ask for a server.
//!
//! Splitting transport choice and scheme choice is the same split the web form uses: the
//! transport enum is the one that has to grow when a new realtime path lands, and the schemes
//! are already expressed at the level both the form and the protocol speak.

use egui::{Align, ComboBox, Layout, RichText, Ui};

use crate::config::{
    default_loopback_server_endpoint, is_loopback_host, parse_host, QuicScheme, RestScheme, Scheme,
    ServerEndpoint, Transport, WsScheme,
};
use crate::theme::{font, palette, space, text_style};
use crate::ui::widgets::ghost_button;

/// What the form is holding locally. Local until the user accepts; the caller's `ServerEndpoint`
/// is the only thing outside the widget's local state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFormState {
    host: String,
    port_text: String,
    gateway_port_text: String,
    transport: Transport,
    scheme: Scheme,
    rest_scheme: RestScheme,
}

impl ServerFormState {
    /// Builds a state from a caller's endpoint, the seed the widget edits.
    pub fn from_endpoint(endpoint: &ServerEndpoint) -> Self {
        Self {
            host: endpoint.host.clone(),
            port_text: endpoint.port.to_string(),
            gateway_port_text: endpoint.gateway_port.to_string(),
            transport: endpoint.transport,
            scheme: endpoint.scheme,
            rest_scheme: endpoint.rest_scheme,
        }
    }
}

impl Default for ServerFormState {
    fn default() -> Self {
        let endpoint = default_loopback_server_endpoint("localhost", 18080);
        Self::from_endpoint(&endpoint)
    }
}

/// Renders the disclosure into `ui`. The widget is a self-contained piece of state: it holds the
/// `ServerFormState` itself (because it owns the disclosure's `open` flag too).
///
/// `value` is the caller's committed endpoint — the thing the one-tap transport selector swaps
/// and the thing the summary line reports. The draft `state` only ever becomes an endpoint
/// through "Use this server".
///
/// Returns an endpoint the caller must apply, or `None`. Two paths produce a value: the
/// transport selector under the toggle (a one-tap swap of the committed endpoint's transport and
/// its paired schemes — everything else rides along untouched), and "Use this server" inside the
/// panel. The caller is responsible for applying the value to its own state and persisting it;
/// the widget is intentionally ignorant of the persistence path so the same shape can be reused
/// on any screen that wants to ask for a server.
pub fn show(
    ui: &mut Ui,
    theme: crate::theme::Theme,
    value: &ServerEndpoint,
    state: &mut ServerFormState,
) -> Option<ServerEndpoint> {
    let colors = palette(theme);
    let mut open = ui
        .data(|data| data.get_temp::<bool>(egui::Id::new("migo-server-disclosure-open")))
        .unwrap_or(false);
    let mut accepted: Option<ServerEndpoint> = None;

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let icon = if open { "▾" } else { "▸" };
            let button = ui.add(
                egui::Button::new(
                    RichText::new(format!("{icon}  Server"))
                        .text_style(crate::theme::named(text_style::OVERLINE))
                        .color(colors.text_muted),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE),
            );
            if button.clicked() {
                open = !open;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{}:{} · {}",
                        state.host,
                        state.port_text,
                        transport_label(value.transport)
                    ))
                    .font(egui::FontId::proportional(font::SMALL))
                    .color(colors.text_muted),
                );
            });
        });
        ui.painter().rect_filled(
            ui.available_rect_before_wrap().intersect(ui.min_rect()).shrink(0.0),
            0,
            egui::Color32::TRANSPARENT,
        );

        // The transport selector is always visible, whether or not the panel is open. One tap
        // swaps the committed endpoint's transport immediately — the host, ports, and everything
        // else ride along untouched, so the choice never waits on "Use this server".
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Transport")
                    .text_style(crate::theme::named(text_style::OVERLINE))
                    .color(colors.text_muted),
            );
            if ui
                .selectable_label(value.transport == Transport::WebSocket, "WebSocket")
                .clicked()
                && value.transport != Transport::WebSocket
            {
                accepted = Some(swap_transport(value, Transport::WebSocket));
            }
            if ui
                .selectable_label(value.transport == Transport::Quic, "QUIC")
                .clicked()
                && value.transport != Transport::Quic
            {
                accepted = Some(swap_transport(value, Transport::Quic));
            }
        });
        if value.transport == Transport::Quic {
            ui.label(
                RichText::new(
                    "QUIC is a second option; it needs a server with the QUIC listener enabled. This client still connects over WebSocket.",
                )
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
            );
        }

        if open {
            ui.indent("migo-server-disclosure-panel", |ui| {
                ui.add_space(space::SM);
                draw_fields(ui, theme, state);
                ui.add_space(space::SM);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ghost_button(ui, theme, "Use this server").clicked() {
                        match build_endpoint(state) {
                            Ok(endpoint) => accepted = Some(endpoint),
                            Err(message) => {
                                ui.label(
                                    RichText::new(message)
                                        .font(egui::FontId::proportional(font::TINY))
                                        .color(colors.danger),
                                );
                            }
                        }
                    }
                });
                ui.add_space(space::MD);
            });
        }
    });

    ui.data_mut(|data| {
        data.insert_temp(egui::Id::new("migo-server-disclosure-open"), open);
    });

    accepted
}

/// Draws the fields plus the scheme picker on the current disclosure. The transport is not here:
/// it lives in the always-visible selector under the toggle, where one tap commits it.
fn draw_fields(ui: &mut Ui, theme: crate::theme::Theme, state: &mut ServerFormState) {
    let colors = palette(theme);

    // Host. The user can still paste `host:port` shorthand into the field; the parser splits it.
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Host")
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text_muted),
        );
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.host)
                .hint_text("migo.example.com")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8)),
        );
        // Match the loopback rule on the fly, the same way the web form does, so the user never
        // sees a "WSS for localhost" placeholder they did not choose.
        if response.changed() {
            let trimmed = state.host.trim().to_ascii_lowercase();
            if state.transport == Transport::WebSocket {
                let pair = schemes_for_host(&state.host);
                state.scheme = pair.scheme;
                state.rest_scheme = pair.rest_scheme;
            }
            let _ = trimmed;
        }
    });

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Port")
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text_muted),
        );
        ui.add(
            egui::TextEdit::singleline(&mut state.port_text)
                .hint_text("18080")
                .desired_width(120.0)
                .margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8)),
        );
        ui.add_space(space::SM);
        ui.label(
            RichText::new("Gateway port")
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text_muted),
        );
        ui.add(
            egui::TextEdit::singleline(&mut state.gateway_port_text)
                .hint_text("18081")
                .desired_width(120.0)
                .margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8)),
        );
    });

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Scheme")
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text_muted),
        );
        let (current_label, options): (&str, Vec<(&str, Scheme, RestScheme)>) =
            match state.transport {
                Transport::WebSocket => (
                    match state.scheme {
                        Scheme::Ws(WsScheme::Wss) => "WSS (TLS)",
                        _ => "WS (plain, dev-only)",
                    },
                    vec![
                        (
                            "WS (plain, dev-only)",
                            Scheme::Ws(WsScheme::Ws),
                            RestScheme::Http,
                        ),
                        ("WSS (TLS)", Scheme::Ws(WsScheme::Wss), RestScheme::Https),
                    ],
                ),
                Transport::Quic => (
                    match state.scheme {
                        Scheme::Quic(crate::config::QuicScheme::QuicTls) => "QUIC-TLS",
                        _ => "QUIC (plain)",
                    },
                    vec![
                        (
                            "QUIC (plain)",
                            Scheme::Quic(crate::config::QuicScheme::Quic),
                            RestScheme::Http,
                        ),
                        (
                            "QUIC-TLS",
                            Scheme::Quic(crate::config::QuicScheme::QuicTls),
                            RestScheme::Https,
                        ),
                    ],
                ),
            };
        ComboBox::from_id_salt("migo-server-scheme")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for (label, scheme, rest_scheme) in &options {
                    if ui
                        .selectable_label(
                            std::mem::discriminant(&state.scheme) == std::mem::discriminant(scheme),
                            *label,
                        )
                        .clicked()
                    {
                        state.scheme = *scheme;
                        state.rest_scheme = *rest_scheme;
                    }
                }
            });
    });
}

/// Picks a default `(scheme, rest_scheme)` pair for a host, matching the web form.
fn schemes_for_host(host: &str) -> SchemeWithRest {
    if is_loopback_host(host) {
        SchemeWithRest {
            scheme: Scheme::Ws(WsScheme::Ws),
            rest_scheme: RestScheme::Http,
        }
    } else {
        SchemeWithRest {
            scheme: Scheme::Ws(WsScheme::Wss),
            rest_scheme: RestScheme::Https,
        }
    }
}

struct SchemeWithRest {
    scheme: Scheme,
    rest_scheme: RestScheme,
}

/// The transport's display name, shared by the summary line and the always-visible selector.
fn transport_label(transport: Transport) -> &'static str {
    match transport {
        Transport::WebSocket => "WebSocket",
        Transport::Quic => "QUIC",
    }
}

/// Builds the endpoint a one-tap transport swap produces: the committed endpoint with the
/// transport and its paired schemes replaced. Host, ports, and everything else ride along
/// untouched — a transport change never needs the rest re-confirmed, which is exactly why the
/// selector commits immediately instead of living in the draft.
fn swap_transport(endpoint: &ServerEndpoint, transport: Transport) -> ServerEndpoint {
    let (scheme, rest_scheme) = schemes_for_transport(transport, &endpoint.host);
    ServerEndpoint {
        transport,
        scheme,
        rest_scheme,
        ..endpoint.clone()
    }
}

/// The default scheme pair for a transport on a given host — the same loopback rule the web and
/// Android forms apply: loopback gets the plain dev pair, everything else the TLS pair.
fn schemes_for_transport(transport: Transport, host: &str) -> (Scheme, RestScheme) {
    match transport {
        Transport::WebSocket => {
            let pair = schemes_for_host(host);
            (pair.scheme, pair.rest_scheme)
        }
        Transport::Quic => {
            if is_loopback_host(host) {
                (Scheme::Quic(QuicScheme::Quic), RestScheme::Http)
            } else {
                (Scheme::Quic(QuicScheme::QuicTls), RestScheme::Https)
            }
        }
    }
}

/// Validates the state and turns it into an endpoint, or returns a form-level error message.
fn build_endpoint(state: &ServerFormState) -> Result<ServerEndpoint, String> {
    if state.host.trim().is_empty() {
        return Err("host is required".to_owned());
    }
    let (host, inline_port) = parse_host(&state.host, 18080).map_err(|error| error.to_string())?;
    let port = if state.port_text.trim().is_empty() {
        if inline_port != 18080 {
            inline_port
        } else {
            return Err("port is required".to_owned());
        }
    } else {
        parse_port(&state.port_text, "port")?
    };
    let gateway_port = if state.gateway_port_text.trim().is_empty() {
        if port > 0 {
            port + 1
        } else {
            1
        }
    } else {
        parse_port(&state.gateway_port_text, "gateway port")?
    };
    let scheme = match state.transport {
        Transport::WebSocket => match state.scheme {
            Scheme::Ws(WsScheme::Ws) | Scheme::Ws(WsScheme::Wss) => state.scheme,
            _ => return Err("WebSocket transport requires WS or WSS scheme".to_owned()),
        },
        Transport::Quic => match state.scheme {
            Scheme::Quic(_) => state.scheme,
            _ => return Err("QUIC transport requires QUIC or QUIC-TLS scheme".to_owned()),
        },
    };
    Ok(ServerEndpoint {
        host,
        port,
        gateway_port,
        transport: state.transport,
        scheme,
        rest_scheme: state.rest_scheme,
    })
}

fn parse_port(raw: &str, label: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required"));
    }
    let value: u16 = trimmed
        .parse()
        .map_err(|_| format!("{label} is not a whole number: {raw}"))?;
    if value == 0 {
        return Err(format!("{label} is out of range (1..65535): {raw}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_endpoint_accepts_a_well_formed_form() {
        let state = ServerFormState {
            host: "migo.example.com".to_owned(),
            port_text: "8443".to_owned(),
            gateway_port_text: "8444".to_owned(),
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Wss),
            rest_scheme: RestScheme::Https,
        };
        let endpoint = build_endpoint(&state).expect("ok");
        assert_eq!(endpoint.host, "migo.example.com");
        assert_eq!(endpoint.port, 8443);
        assert_eq!(endpoint.gateway_port, 8444);
    }

    #[test]
    fn build_endpoint_rejects_an_empty_host() {
        let state = ServerFormState {
            host: "   ".to_owned(),
            port_text: "8443".to_owned(),
            gateway_port_text: "8444".to_owned(),
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Wss),
            rest_scheme: RestScheme::Https,
        };
        let error = build_endpoint(&state).expect_err("should reject");
        assert!(error.contains("host"), "got {error}");
    }

    #[test]
    fn build_endpoint_rejects_a_port_out_of_range() {
        let state = ServerFormState {
            host: "migo.example.com".to_owned(),
            port_text: "0".to_owned(),
            gateway_port_text: "8444".to_owned(),
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Wss),
            rest_scheme: RestScheme::Https,
        };
        let error = build_endpoint(&state).expect_err("should reject");
        assert!(error.contains("port"), "got {error}");
    }

    #[test]
    fn build_endpoint_rejects_a_scheme_transport_mismatch() {
        let state = ServerFormState {
            host: "migo.example.com".to_owned(),
            port_text: "8443".to_owned(),
            gateway_port_text: "8444".to_owned(),
            transport: Transport::WebSocket,
            scheme: Scheme::Quic(crate::config::QuicScheme::Quic),
            rest_scheme: RestScheme::Https,
        };
        let error = build_endpoint(&state).expect_err("should reject");
        assert!(error.contains("WS or WSS"), "got {error}");
    }

    #[test]
    fn build_endpoint_accepts_a_host_port_shorthand_when_port_field_is_blank() {
        let state = ServerFormState {
            host: "migo.example.com:8443".to_owned(),
            port_text: "".to_owned(),
            gateway_port_text: "8444".to_owned(),
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Wss),
            rest_scheme: RestScheme::Https,
        };
        let endpoint = build_endpoint(&state).expect("ok");
        assert_eq!(endpoint.host, "migo.example.com");
        assert_eq!(endpoint.port, 8443);
    }

    #[test]
    fn a_transport_swap_to_quic_pairs_the_tls_schemes_on_a_public_host() {
        let endpoint = ServerEndpoint {
            host: "152.53.102.150".to_owned(),
            port: 8080,
            gateway_port: 8081,
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Ws),
            rest_scheme: RestScheme::Http,
        };
        let swapped = swap_transport(&endpoint, Transport::Quic);
        assert_eq!(swapped.transport, Transport::Quic);
        assert_eq!(swapped.scheme, Scheme::Quic(QuicScheme::QuicTls));
        assert_eq!(swapped.rest_scheme, RestScheme::Https);
        // The swap touches only the transport and its schemes; the addressing rides along.
        assert_eq!(swapped.host, endpoint.host);
        assert_eq!(swapped.port, endpoint.port);
        assert_eq!(swapped.gateway_port, endpoint.gateway_port);
    }

    #[test]
    fn a_transport_swap_to_quic_keeps_the_plain_pair_on_loopback() {
        let endpoint = ServerEndpoint {
            host: "localhost".to_owned(),
            port: 18080,
            gateway_port: 18081,
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Ws),
            rest_scheme: RestScheme::Http,
        };
        let swapped = swap_transport(&endpoint, Transport::Quic);
        assert_eq!(swapped.scheme, Scheme::Quic(QuicScheme::Quic));
        assert_eq!(swapped.rest_scheme, RestScheme::Http);
    }

    #[test]
    fn a_transport_swap_back_to_websocket_restores_the_host_pair() {
        let endpoint = ServerEndpoint {
            host: "migo.example.com".to_owned(),
            port: 8443,
            gateway_port: 8444,
            transport: Transport::Quic,
            scheme: Scheme::Quic(QuicScheme::QuicTls),
            rest_scheme: RestScheme::Https,
        };
        let swapped = swap_transport(&endpoint, Transport::WebSocket);
        assert_eq!(swapped.transport, Transport::WebSocket);
        assert_eq!(swapped.scheme, Scheme::Ws(WsScheme::Wss));
        assert_eq!(swapped.rest_scheme, RestScheme::Https);
    }
}
