//! The "Server" disclosure on the sign-in and registration forms.
//!
//! A user who has never opened this disclosure sees exactly the form they saw yesterday: identifier
//! and passphrase. A user who has opened it picks the host, port and scheme, and on
//! "Use this server" the disclosure closes and the choice becomes the new form input.
//!
//! The mode row inside the panel is the one addition that changes what the panel is: "Otomatis"
//! probes every candidate node (`MIGO_SERVERS`, else the public deployment) in parallel on
//! `GET /health` and adopts the fastest responder, while "Manual" is the host-and-port form that
//! has always been here. Auto shows the node it resolved beside the choice, so the person never
//! wonders which node answered; and when no node answers, auto stays chosen and the failure
//! surfaces through the connection status — never as a silently pinned dead server. The form
//! itself never probes: it raises a flag the shell turns into a probe thread, because nothing in
//! `ui` may reach a socket.
//!
//! The transport is the one choice that is not behind the disclosure: a TCP/WebSocket/QUIC row of
//! selectable labels rides directly under the toggle and one click commits the swap immediately —
//! a transport change never needs the host and port re-confirmed, so it never lives in the draft.
//! TCP is the native default (one socket, one session, length-prefixed binary frames — the
//! mig33v46 heritage); WebSocket is the web client's transport, kept here for development and
//! fallback; QUIC is the second option, negotiated via the `QUIC` feature bit. When a picked
//! transport is not negotiated the worker falls back to WebSocket and says so plainly in the
//! connection state. The form itself never blocks submit.
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
    default_loopback_server_endpoint, is_loopback_host, parse_host, rest_base_url, QuicScheme,
    RestScheme, Scheme, ServerEndpoint, TcpScheme, Transport, WsScheme,
};
use crate::theme::{font, palette, space, text_style};
use crate::ui::widgets::ghost_button;

/// What the auto choice is doing right now. Held by the caller between frames because the probe
/// outlives the frame that asked for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AutoStatus {
    /// Auto is not chosen: the manual host-and-port form is the interface.
    #[default]
    Off,
    /// The probes are in flight. The shell spawns them and files the answer back here.
    Probing,
    /// Auto resolved to this node — the endpoint the panel shows beside the choice.
    Resolved(ServerEndpoint),
    /// No candidate answered the health probe. Auto stays chosen; the last accepted server is
    /// still the one the client tries, and the connection status reports its failure.
    Failed,
}

/// The mode choice's cross-frame state: what auto is doing, and the flag the form raises when
/// the user picks "Otomatis".
///
/// The form cannot probe by itself — nothing in `ui` is given a socket — so it sets
/// `probe_requested` and the shell spawns the probe thread, delivers the answer on a channel,
/// and clears the flag. One flag rather than a channel here keeps this struct plain data the
/// caller can hold next to its other form state.
#[derive(Debug, Default)]
pub struct ServerChoiceState {
    /// The auto choice's current standing.
    pub auto: AutoStatus,
    /// Raised the frame the user picks "Otomatis"; the shell spawns the probes and clears it.
    pub probe_requested: bool,
}

impl ServerChoiceState {
    /// Whether the auto choice is on — probing, resolved, or failed all count, because all three
    /// are "the user asked auto to pick" rather than "the user is typing a host".
    pub fn auto_on(&self) -> bool {
        self.auto != AutoStatus::Off
    }
}

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
/// through "Use this server". `choice` is the mode choice's cross-frame state: the form reads it
/// to draw the auto standing and writes it when the user flips the mode or accepts an endpoint
/// by hand, which always leaves auto — a hand-accepted server is a manual choice, whatever was
/// resolved before it.
///
/// Returns an endpoint the caller must apply, or `None`. Two paths produce a value: the
/// transport selector under the toggle (a one-tap swap of the committed endpoint's transport and
/// its paired schemes — everything else rides along untouched), and "Use this server" inside the
/// panel. The caller is responsible for applying the value to its own state and persisting it;
/// the widget is intentionally ignorant of the persistence path so the same shape can be reused
/// on any screen that wants to ask for a server. The auto path never returns here: its endpoint
/// arrives from the shell when the probe answers, and lands in `value` the same way.
pub fn show(
    ui: &mut Ui,
    theme: crate::theme::Theme,
    value: &ServerEndpoint,
    state: &mut ServerFormState,
    choice: &mut ServerChoiceState,
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
                // In auto mode the draft is not the interface — the committed value is, because
                // the committed value is the node auto resolved.
                let (host, port) = if choice.auto_on() {
                    (value.host.as_str(), value.port)
                } else {
                    (state.host.as_str(), state.port_text.parse().unwrap_or(value.port))
                };
                ui.label(
                    RichText::new(format!(
                        "{}:{} · {}",
                        host,
                        port,
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
                .selectable_label(value.transport == Transport::Tcp, "TCP")
                .clicked()
                && value.transport != Transport::Tcp
            {
                accepted = Some(swap_transport(value, Transport::Tcp));
            }
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
        match value.transport {
            Transport::Tcp => {
                ui.label(
                    RichText::new(
                        "TCP is the native default: one socket, one session, binary length-prefixed frames. It needs a server with the TCP listener enabled; if the server does not offer it, this client falls back to WebSocket and says so.",
                    )
                    .font(egui::FontId::proportional(font::TINY))
                    .color(colors.text_muted),
                );
            }
            Transport::Quic => {
                ui.label(
                    RichText::new(
                        "QUIC is a second option; it needs a server with the QUIC listener enabled. If the server does not offer it, this client falls back to WebSocket and says so.",
                    )
                    .font(egui::FontId::proportional(font::TINY))
                    .color(colors.text_muted),
                );
            }
            Transport::WebSocket => {}
        }

        if open {
            ui.indent("migo-server-disclosure-panel", |ui| {
                ui.add_space(space::SM);
                mode_row(ui, theme, choice);
                ui.add_space(space::SM);
                if choice.auto_on() {
                    auto_standing(ui, theme, &choice.auto);
                } else {
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
                }
                ui.add_space(space::MD);
            });
        }
    });

    // A hand-accepted endpoint always leaves auto: the person overruled the probe, and the
    // persisted mode must say so rather than letting the next launch re-resolve over their
    // choice.
    if accepted.is_some() {
        choice.auto = AutoStatus::Off;
    }

    ui.data_mut(|data| {
        data.insert_temp(egui::Id::new("migo-server-disclosure-open"), open);
    });

    accepted
}

/// The mode row: "Manual" and "Otomatis" as one-tap selected labels, the same shape the
/// transport selector takes. Committing "Otomatis" raises the probe flag — the shell owns the
/// probing — and committing "Manual" only flips the standing, because the host-and-port form
/// below it commits through "Use this server" as it always has.
fn mode_row(ui: &mut Ui, theme: crate::theme::Theme, choice: &mut ServerChoiceState) {
    let colors = palette(theme);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Mode")
                .text_style(crate::theme::named(text_style::OVERLINE))
                .color(colors.text_muted),
        );
        if ui.selectable_label(!choice.auto_on(), "Manual").clicked() && choice.auto_on() {
            choice.auto = AutoStatus::Off;
        }
        if ui
            .selectable_label(choice.auto_on(), "Otomatis")
            .on_hover_text(
                "Probes every candidate node with GET /health and takes the fastest responder. \
                 The candidates come from MIGO_SERVERS, or the public deployment when unset.",
            )
            .clicked()
            // Clickable when auto is off, and again once a probe has failed: a failed standing
            // is still "auto is on", but the one action it owes is another try.
            && (!choice.auto_on() || matches!(choice.auto, AutoStatus::Failed))
        {
            choice.auto = AutoStatus::Probing;
            choice.probe_requested = true;
        }
    });
}

/// The auto choice's standing, drawn where the manual fields would be. The resolved endpoint is
/// shown in full beside the choice, because "which node did auto pick?" is the one question the
/// choice owes an answer to; a failure says so and names the fallback, rather than pretending a
/// dead server was chosen.
fn auto_standing(ui: &mut Ui, theme: crate::theme::Theme, status: &AutoStatus) {
    let colors = palette(theme);
    match status {
        AutoStatus::Off => {}
        AutoStatus::Probing => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new("Memeriksa server kandidat\u{2026}")
                        .font(egui::FontId::proportional(font::SMALL))
                        .color(colors.text_muted),
                );
            });
            ui.label(
                RichText::new(
                    "Every candidate is asked GET /health at once, three seconds each; the \
                     fastest 2xx answer wins.",
                )
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
            );
        }
        AutoStatus::Resolved(endpoint) => {
            ui.label(
                RichText::new(rest_base_url(endpoint))
                    .font(egui::FontId::monospace(font::SMALL))
                    .color(colors.text),
            );
            ui.label(
                RichText::new(
                    "The fastest node that answered the health probe. It is re-chosen at every \
                     launch; pick Manual to pin a server by hand.",
                )
                .font(egui::FontId::proportional(font::TINY))
                .color(colors.text_muted),
            );
        }
        AutoStatus::Failed => {
            ui.label(
                RichText::new(
                    "No candidate answered the health probe. The last accepted server stays in \
                     use and its failure shows in the connection status — auto never pins a \
                     server it could not reach. Pick Manual to type one by hand, or choose \
                     Otomatis again to retry.",
                )
                .font(egui::FontId::proportional(font::SMALL))
                .color(colors.warning),
            );
        }
    }
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
                Transport::Tcp => (
                    match state.scheme {
                        Scheme::Tcp(TcpScheme::TcpTls) => "TCP-TLS",
                        _ => "TCP (plain, dev-only)",
                    },
                    vec![
                        (
                            "TCP (plain, dev-only)",
                            Scheme::Tcp(TcpScheme::Tcp),
                            RestScheme::Http,
                        ),
                        ("TCP-TLS", Scheme::Tcp(TcpScheme::TcpTls), RestScheme::Https),
                    ],
                ),
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
        Transport::Tcp => "TCP",
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
        Transport::Tcp => {
            if is_loopback_host(host) {
                (Scheme::Tcp(TcpScheme::Tcp), RestScheme::Http)
            } else {
                (Scheme::Tcp(TcpScheme::TcpTls), RestScheme::Https)
            }
        }
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
        Transport::Tcp => match state.scheme {
            Scheme::Tcp(_) => state.scheme,
            _ => return Err("TCP transport requires TCP or TCP-TLS scheme".to_owned()),
        },
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

    /// Every standing except `Off` is "auto is on", because probing, resolved, and failed are
    /// all "the user asked auto to pick" — the mode row's selected label and the shell's
    /// persisted mode both read it that way, and the two must not drift.
    #[test]
    fn every_standing_except_off_means_auto_is_on() {
        let mut choice = ServerChoiceState::default();
        assert!(!choice.auto_on());
        for standing in [
            AutoStatus::Probing,
            AutoStatus::Failed,
            AutoStatus::Resolved(crate::config::default_production_server_endpoint()),
        ] {
            choice.auto = standing;
            assert!(choice.auto_on());
        }
        choice.auto = AutoStatus::Off;
        assert!(!choice.auto_on());
    }

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
