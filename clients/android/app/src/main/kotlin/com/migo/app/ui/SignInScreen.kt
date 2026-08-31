package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.core.store.GatewayScheme
import com.migo.core.store.RestScheme
import com.migo.core.store.ServerEndpoint
import com.migo.core.store.Transport

/**
 * The sign-in and registration form, which are one screen because they differ by one button.
 *
 * Two forms would mean two copies of the server field, the account field and the password field, kept
 * consistent by hand, to express a difference the server treats as two endpoints and a person treats
 * as "I have an account" or "I do not".
 *
 * The front door is the reference's: the cyan gradient fills the screen in either theme — the sign-in
 * surface is the one place that ignores the lights — with the brand on the gradient and the form on a
 * nearly-opaque card, so the fields keep the palette ink they already had. The submit button is the
 * banner orange, the one warm accent the identity carries.
 *
 * The server field is a disclosure: collapsed by default, with a summary line ("localhost:18080",
 * say) that shows the current value, and an expanded panel underneath that exposes the structured
 * fields. The disclosure is the same shape the web and desktop clients ship — a self-hoster who
 * picks `migo.example.com:8443` here picks the same fields on every client.
 *
 * # The password never leaves this function
 *
 * It lives in a [remember] here -- not [rememberSaveable], so it is not written into the saved-state
 * bundle the system may persist to disk -- and is handed to [onSubmit] as an argument. There is no
 * field for it on [AppState], which is what keeps it out of every recomposition, every log line, and
 * every future `toString`.
 */
@Composable
fun SignInScreen(
    form: AppState.SignedOut,
    onServerEndpoint: (ServerEndpoint) -> Unit,
    onIdentifier: (String) -> Unit,
    onSubmit: (password: String, create: Boolean) -> Unit,
    onDismissFailure: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var password by remember { mutableStateOf("") }
    // Whether the person is registering does survive a rotation: it is a choice they made, and not a
    // credential.
    var creating by rememberSaveable { mutableStateOf(false) }
    val extra = LocalMigoExtra.current

    Box(
        modifier = modifier
            .fillMaxSize()
            .background(
                brush = Brush.verticalGradient(
                    listOf(extra.loginA, extra.loginB, extra.loginC),
                ),
            ),
    ) {
        Column(modifier = Modifier.fillMaxSize()) {
            ErrorBanner(message = form.failure, onDismiss = onDismissFailure)
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .imePadding()
                    .padding(horizontal = 24.dp, vertical = 24.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    text = "Migo",
                    style = MaterialTheme.typography.displaySmall,
                    fontWeight = FontWeight.Bold,
                    color = extra.bannerInk,
                )
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "End-to-end encrypted. Your keys stay on this device.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = extra.bannerInk.copy(alpha = 0.9f),
                    textAlign = TextAlign.Center,
                )
                Spacer(modifier = Modifier.height(32.dp))

                // The form sits on a card rather than on the gradient: a translucent-enough card to
                // read as the reference's glass, opaque enough that the fields keep the contrast
                // their labels were measured against.
                Surface(
                    color = MaterialTheme.colorScheme.surface.copy(alpha = 0.94f),
                    shape = MaterialTheme.shapes.large,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(modifier = Modifier.padding(20.dp)) {
                        ServerDisclosure(
                            value = form.serverEndpoint,
                            enabled = !form.busy,
                            onCommit = onServerEndpoint,
                        )
                        Spacer(modifier = Modifier.height(12.dp))

                        OutlinedTextField(
                            value = form.identifier,
                            onValueChange = onIdentifier,
                            label = { Text(if (creating) "Choose a username" else "Username or email") },
                            singleLine = true,
                            enabled = !form.busy,
                            keyboardOptions = KeyboardOptions(
                                keyboardType = KeyboardType.Email,
                                imeAction = ImeAction.Next,
                            ),
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(modifier = Modifier.height(12.dp))

                        OutlinedTextField(
                            value = password,
                            onValueChange = { password = it },
                            label = { Text("Password") },
                            singleLine = true,
                            enabled = !form.busy,
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(
                                keyboardType = KeyboardType.Password,
                                imeAction = ImeAction.Done,
                            ),
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(modifier = Modifier.height(24.dp))

                        Button(
                            onClick = { onSubmit(password, creating) },
                            enabled = !form.busy,
                            colors = ButtonDefaults.buttonColors(
                                containerColor = extra.bannerB,
                                contentColor = extra.bannerInk,
                            ),
                            modifier = Modifier.fillMaxWidth().height(52.dp),
                        ) {
                            if (form.busy) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    CircularProgressIndicator(
                                        modifier = Modifier.size(18.dp),
                                        strokeWidth = 2.dp,
                                        color = extra.bannerInk,
                                    )
                                    Spacer(modifier = Modifier.width(12.dp))
                                    Text(if (creating) "Creating account" else "Signing in")
                                }
                            } else {
                                Text(if (creating) "Create account" else "Sign in")
                            }
                        }
                        Spacer(modifier = Modifier.height(4.dp))

                        TextButton(onClick = { creating = !creating }, enabled = !form.busy) {
                            Text(
                                text = if (creating) {
                                    "I already have an account"
                                } else {
                                    "Create a new account"
                                },
                            )
                        }
                        Spacer(modifier = Modifier.height(8.dp))

                        if (creating) {
                            Text(
                                text = "Your identity key is generated here and never sent to the server. " +
                                    "Signing out destroys it.",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                }
            }
        }
    }
}

/**
 * The "Server" disclosure, mirroring `clients/web/src/components/server-form.tsx`.
 *
 * A user who has never opened it sees a single summary line ("localhost:18080", say)
 * and a chevron; opening it reveals the structured fields. The form's working state
 * stays local until "Use this server" is clicked -- so a half-typed entry is not
 * pushed into the bootstrap, and a failed sign-in does not have to re-fetch a
 * not-yet-confirmed host.
 *
 * The form holds its own local state for every field; the view model is only asked
 * for the previous value (to initialise the draft) and is given a new record on
 * commit. The summary line uses the local state so it updates as the user types.
 */
@Composable
private fun ServerDisclosure(
    value: ServerEndpoint,
    enabled: Boolean,
    onCommit: (ServerEndpoint) -> Unit,
) {
    var open by rememberSaveable { mutableStateOf(false) }
    // A local draft so partial edits are not pushed to the view model until the
    // user clicks "Use this server". The disclosure is initialised from the current
    // endpoint every time it is opened.
    var host by rememberSaveable(value) { mutableStateOf(value.host) }
    var port by rememberSaveable(value) { mutableStateOf(value.port.toString()) }
    var gatewayPort by rememberSaveable(value) { mutableStateOf(value.gatewayPort.toString()) }
    var transport by rememberSaveable(value) { mutableStateOf(value.transport) }
    var gatewayScheme by rememberSaveable(value) { mutableStateOf(value.gatewayScheme) }
    var restScheme by rememberSaveable(value) { mutableStateOf(value.restScheme) }
    var error by remember { mutableStateOf<String?>(null) }

    Column(
        modifier = Modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.Start,
    ) {
        TextButton(
            onClick = { open = !open },
            enabled = enabled,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(text = if (open) "▾" else "▸")
                Spacer(modifier = Modifier.width(6.dp))
                Text(text = "Server")
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = serverSummary(host, port, gatewayPort),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (open) {
            Column(
                modifier = Modifier.fillMaxWidth().padding(start = 8.dp, end = 8.dp, bottom = 8.dp),
            ) {
                OutlinedTextField(
                    value = host,
                    onValueChange = { host = it },
                    label = { Text("Host") },
                    placeholder = { Text("migo.example.com") },
                    singleLine = true,
                    enabled = enabled,
                    keyboardOptions = KeyboardOptions(
                        keyboardType = KeyboardType.Uri,
                        imeAction = ImeAction.Next,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(modifier = Modifier.height(8.dp))
                Row(modifier = Modifier.fillMaxWidth()) {
                    OutlinedTextField(
                        value = port,
                        onValueChange = { port = it.filter { ch -> ch.isDigit() }.take(5) },
                        label = { Text("Port") },
                        placeholder = { Text("18080") },
                        singleLine = true,
                        enabled = enabled,
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Number,
                            imeAction = ImeAction.Next,
                        ),
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    OutlinedTextField(
                        value = gatewayPort,
                        onValueChange = { gatewayPort = it.filter { ch -> ch.isDigit() }.take(5) },
                        label = { Text("Gateway port") },
                        placeholder = { Text("(REST + 1)") },
                        singleLine = true,
                        enabled = enabled,
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Number,
                            imeAction = ImeAction.Next,
                        ),
                        modifier = Modifier.weight(1f),
                    )
                }
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "Transport",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TransportChoice(
                    value = transport,
                    enabled = enabled,
                    onChange = { transport = it },
                )
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "Scheme",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                SchemeChoice(
                    transport = transport,
                    gatewayScheme = gatewayScheme,
                    restScheme = restScheme,
                    enabled = enabled,
                    onGateway = { gatewayScheme = it },
                    onRest = { restScheme = it },
                )
                if (transport == Transport.Quic) {
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = "QUIC support is coming soon. Pick WebSocket to sign in today.",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                error?.let {
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                Spacer(modifier = Modifier.height(8.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End,
                ) {
                    TextButton(
                        onClick = {
                            try {
                                val next = buildFromDraft(
                                    host = host,
                                    port = port,
                                    gatewayPort = gatewayPort,
                                    transport = transport,
                                    gatewayScheme = gatewayScheme,
                                    restScheme = restScheme,
                                )
                                error = null
                                onCommit(next)
                                open = false
                            } catch (cause: Exception) {
                                error = cause.message ?: "invalid server"
                            }
                        },
                        enabled = enabled,
                    ) {
                        Text("Use this server")
                    }
                }
            }
        }
    }
}

/** The single-line summary shown when the disclosure is collapsed. */
private fun serverSummary(host: String, port: String, gatewayPort: String): String {
    val h = host.ifBlank { "unset" }
    val p = port.ifBlank { "?" }
    val g = if (gatewayPort.isBlank()) "auto" else gatewayPort
    return "$h:$p  ·  gateway $g"
}

/**
 * The transport picker: WebSocket wired, QUIC visible but disabled. Mirrors the web form's
 * behaviour exactly, so a user who picked "QUIC (coming soon)" on the web and picks it on
 * Android sees the same label.
 */
@Composable
private fun TransportChoice(
    value: Transport,
    enabled: Boolean,
    onChange: (Transport) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Transport.values().forEach { option ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(end = 16.dp),
            ) {
                androidx.compose.material3.RadioButton(
                    selected = value == option,
                    onClick = { if (enabled && option != Transport.Quic) onChange(option) },
                    enabled = enabled && option != Transport.Quic,
                )
                Text(
                    text = if (option == Transport.Quic) "QUIC (coming soon)" else "WebSocket",
                    style = MaterialTheme.typography.bodyMedium,
                    color = if (option == Transport.Quic) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                )
            }
        }
    }
}

/**
 * The scheme picker: HTTP for loopback, HTTPS otherwise. The gateway scheme is paired
 * with the REST one: plain HTTP for the REST plane implies plain WS for the gateway,
 * HTTPS implies WSS. The picker lets a user override either side when the deployment
 * is split (TLS terminator in front of plain migod, say), at the cost of seeing a
 * result that the dev policy would have rejected as a default.
 */
@Composable
private fun SchemeChoice(
    transport: Transport,
    gatewayScheme: GatewayScheme,
    restScheme: RestScheme,
    enabled: Boolean,
    onGateway: (GatewayScheme) -> Unit,
    onRest: (RestScheme) -> Unit,
) {
    Column {
        // REST scheme (HTTP / HTTPS).
        Row(verticalAlignment = Alignment.CenterVertically) {
            RestScheme.values().forEach { option ->
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.padding(end = 16.dp),
                ) {
                    androidx.compose.material3.RadioButton(
                        selected = restScheme == option,
                        onClick = {
                            if (!enabled) return@RadioButton
                            onRest(option)
                            // Keep the gateway scheme paired: HTTP -> WS, HTTPS -> WSS.
                            onGateway(
                                if (option == RestScheme.Https) {
                                    GatewayScheme.Wss
                                } else {
                                    GatewayScheme.Ws
                                },
                            )
                        },
                        enabled = enabled,
                    )
                    Text(
                        text = if (option == RestScheme.Https) {
                            "HTTPS (everywhere else)"
                        } else {
                            "HTTP (loopback only)"
                        },
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        }
        // Gateway scheme, kept paired but exposed for the reverse-proxy case.
        if (transport == Transport.WebSocket) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                GatewayScheme.values().forEach { option ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(end = 16.dp),
                    ) {
                        androidx.compose.material3.RadioButton(
                            selected = gatewayScheme == option,
                            onClick = { if (enabled) onGateway(option) },
                            enabled = enabled,
                        )
                        Text(
                            text = if (option == GatewayScheme.Wss) "WSS" else "WS",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
            }
        }
    }
}

/**
 * Validates the form's local state and returns a [ServerEndpoint].
 *
 * Throws on any failure, with a message the disclosure can show verbatim. The same
 * checks the web form runs (`packages/sdk/src/server-endpoint.ts`); a value that
 * validates here is one a `MigoClient.create` call will accept.
 */
private fun buildFromDraft(
    host: String,
    port: String,
    gatewayPort: String,
    transport: Transport,
    gatewayScheme: GatewayScheme,
    restScheme: RestScheme,
): ServerEndpoint {
    val trimmedHost = host.trim()
    if (trimmedHost.isEmpty()) {
        throw IllegalArgumentException("host is required")
    }
    val parsedPort = parsePort(port, "port")
    val parsedGatewayPort = if (gatewayPort.isBlank()) {
        // The form's "use REST + 1" helper. The dev default; the structured record's
        // own defaults are overridden for the same reason the helper exists.
        if (parsedPort >= 65535) parsedPort else parsedPort + 1
    } else {
        parsePort(gatewayPort, "gateway port")
    }
    if (transport == Transport.Quic) {
        // The form already disables the QUIC option, but a user who found a way to
        // pick it (custom input, a future build that unblocks it) gets the same
        // message the form shows.
        throw IllegalArgumentException("QUIC support is coming soon. Pick WebSocket to sign in today.")
    }
    return ServerEndpoint(
        host = trimmedHost.lowercase(),
        port = parsedPort,
        gatewayPort = parsedGatewayPort,
        transport = transport,
        gatewayScheme = gatewayScheme,
        restScheme = restScheme,
    )
}

private fun parsePort(raw: String, label: String): Int {
    val trimmed = raw.trim()
    if (trimmed.isEmpty()) throw IllegalArgumentException("$label is required")
    val value = trimmed.toIntOrNull()
        ?: throw IllegalArgumentException("$label is not a number: $raw")
    if (value < 1 || value > 65535) {
        throw IllegalArgumentException("$label is out of range (1..65535): $raw")
    }
    return value
}
