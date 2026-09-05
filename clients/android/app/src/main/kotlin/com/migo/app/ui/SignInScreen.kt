package com.migo.app.ui

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextButtonDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.app.model.AppState
import com.migo.core.store.GatewayScheme
import com.migo.core.store.RestScheme
import com.migo.core.store.ServerEndpoint
import com.migo.core.store.Transport

/**
 * The sign-in and registration form, which are one screen because they differ by one button.
 *
 * Two forms would mean two copies of the server field, the account field and the passphrase field, kept
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
 * # The passphrase never leaves this function
 *
 * It lives in a [remember] here -- not [rememberSaveable], so it is not written into the saved-state
 * bundle the system may persist to disk -- and is handed to [onSubmit] as an argument. The same
 * rule covers the backup's recovery credential on the restore path, handed to [onRestore]. There
 * is no field for either on [AppState], which is what keeps them out of every recomposition, every
 * log line, and every future `toString`.
 *
 * # The third door: restoring a `.migo` container
 *
 * A person holding an account backup joins the account from this screen too: the restore mode
 * swaps the passphrase field for the container's recovery credential and adds the file picker
 * above the submit. The username field becomes the greeting only -- optional, and defaulted to
 * the account's public id by the session layer when left blank -- because the grant names the
 * account, not anything typed here.
 */
@Composable
fun SignInScreen(
    form: AppState.SignedOut,
    onServerEndpoint: (ServerEndpoint) -> Unit,
    onIdentifier: (String) -> Unit,
    onSubmit: (passphrase: String, create: Boolean) -> Unit,
    onRestore: (container: Uri, credential: String) -> Unit,
    onDismissFailure: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var passphrase by remember { mutableStateOf("") }
    // Whether the person is registering does survive a rotation: it is a choice they made, and not a
    // credential. The restore mode's choice rides with it for the same reason; the chosen file's
    // Uri does not, because a picker grant does not survive the process either.
    var creating by rememberSaveable { mutableStateOf(false) }
    var restoring by rememberSaveable { mutableStateOf(false) }
    var containerUri by remember { mutableStateOf<Uri?>(null) }
    val pickContainer = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { chosen ->
        if (chosen != null) containerUri = chosen
    }
    val extra = LocalMigoExtra.current

    // The front door's ground: the reference's flat turquoise. It still goes through a brush
    // because the tokens are three stops, but the three stops are equal now, so what lands on the
    // screen is one flat colour — the restyle's rule, kept honest by the palette rather than by a
    // rewrite of every front-door call site.
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

                // The form sits on a flat card: the reference's darker turquoise, separated from
                // the ground by its own colour and a translucent 1px border rather than by any
                // elevation. Everything inside inherits white ink from the card's content color,
                // so the labels and the toggles need no colour of their own.
                Surface(
                    color = Color(0xFF0B6F82),
                    contentColor = Color.White,
                    shape = RoundedCornerShape(16.dp),
                    border = BorderStroke(1.dp, Color.White.copy(alpha = 0.28f)),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(modifier = Modifier.padding(20.dp)) {
                        ServerDisclosure(
                            value = form.serverEndpoint,
                            enabled = !form.busy,
                            onCommit = onServerEndpoint,
                        )
                        Spacer(modifier = Modifier.height(12.dp))

                        val identifierLabel = when {
                            creating -> "Choose a username"
                            restoring -> "Username (optional)"
                            else -> "Username or email"
                        }
                        AuthLabel(text = identifierLabel)
                        Spacer(modifier = Modifier.height(6.dp))
                        OutlinedTextField(
                            value = form.identifier,
                            onValueChange = onIdentifier,
                            placeholder = { Text(identifierLabel) },
                            singleLine = true,
                            enabled = !form.busy,
                            shape = RoundedCornerShape(8.dp),
                            colors = authFieldColors(),
                            keyboardOptions = KeyboardOptions(
                                keyboardType = KeyboardType.Email,
                                imeAction = ImeAction.Next,
                            ),
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(modifier = Modifier.height(12.dp))

                        val passphraseLabel = if (restoring) "Recovery credential" else "Passphrase"
                        AuthLabel(text = passphraseLabel)
                        Spacer(modifier = Modifier.height(6.dp))
                        OutlinedTextField(
                            value = passphrase,
                            onValueChange = { passphrase = it },
                            placeholder = { Text(passphraseLabel) },
                            singleLine = true,
                            enabled = !form.busy,
                            visualTransformation = PasswordVisualTransformation(),
                            shape = RoundedCornerShape(8.dp),
                            colors = authFieldColors(),
                            keyboardOptions = KeyboardOptions(
                                keyboardType = KeyboardType.Password,
                                imeAction = ImeAction.Done,
                            ),
                            modifier = Modifier.fillMaxWidth(),
                        )
                        if (restoring) {
                            Spacer(modifier = Modifier.height(12.dp))
                            OutlinedButton(
                                onClick = { pickContainer.launch(arrayOf("*/*")) },
                                enabled = !form.busy,
                                colors = ButtonDefaults.outlinedButtonColors(
                                    contentColor = Color.White,
                                ),
                                border = BorderStroke(1.dp, Color.White.copy(alpha = 0.5f)),
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text(
                                    text = containerUri
                                        ?.let { "Backup: ${it.lastPathSegment ?: "chosen"}" }
                                        ?: "Choose the .migo backup file",
                                    maxLines = 1,
                                )
                            }
                        }
                        Spacer(modifier = Modifier.height(24.dp))

                        Button(
                            onClick = {
                                if (restoring) {
                                    containerUri?.let { onRestore(it, passphrase) }
                                } else {
                                    onSubmit(passphrase, creating)
                                }
                            },
                            enabled = !form.busy && (!restoring || containerUri != null),
                            shape = RoundedCornerShape(8.dp),
                            colors = ButtonDefaults.buttonColors(
                                containerColor = extra.bannerA,
                                contentColor = extra.bannerInk,
                                disabledContainerColor = Color(0xFF8A8C50),
                                disabledContentColor = extra.bannerInk,
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
                                    Text(
                                        when {
                                            creating -> "Creating account"
                                            restoring -> "Restoring account"
                                            else -> "Signing in"
                                        },
                                    )
                                }
                            } else {
                                Text(if (restoring) "Restore account" else if (creating) "Create account" else "Sign in")
                            }
                        }
                        Spacer(modifier = Modifier.height(4.dp))

                        when {
                            creating -> TextButton(
                                onClick = { creating = false },
                                enabled = !form.busy,
                                colors = TextButtonDefaults.textButtonColors(
                                    contentColor = Color.White.copy(alpha = 0.9f),
                                ),
                            ) {
                                Text("I already have an account")
                            }

                            restoring -> TextButton(
                                onClick = { restoring = false },
                                enabled = !form.busy,
                                colors = TextButtonDefaults.textButtonColors(
                                    contentColor = Color.White.copy(alpha = 0.9f),
                                ),
                            ) {
                                Text("Sign in instead")
                            }

                            else -> Row {
                                TextButton(
                                    onClick = { creating = true },
                                    enabled = !form.busy,
                                    colors = TextButtonDefaults.textButtonColors(
                                        contentColor = Color.White.copy(alpha = 0.9f),
                                    ),
                                ) {
                                    Text("Create a new account")
                                }
                                TextButton(
                                    onClick = { restoring = true },
                                    enabled = !form.busy,
                                    colors = TextButtonDefaults.textButtonColors(
                                        contentColor = Color.White.copy(alpha = 0.9f),
                                    ),
                                ) {
                                    Text("Restore from a backup")
                                }
                            }
                        }
                        Spacer(modifier = Modifier.height(8.dp))

                        if (creating) {
                            Text(
                                text = "Your identity key is generated here and never sent to the server. " +
                                    "Signing out destroys it.",
                                fontSize = 12.sp,
                                color = Color.White.copy(alpha = 0.82f),
                                textAlign = TextAlign.Center,
                            )
                        }
                        if (restoring) {
                            Text(
                                text = "The backup carries your account root. This device joins the account " +
                                    "as a new device, with a fresh identity of its own.",
                                fontSize = 12.sp,
                                color = Color.White.copy(alpha = 0.82f),
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                }
            }
        }
    }
}

/** A form label on the auth card: white, 13sp, bold — the one weight the front door uses. */
@Composable
private fun AuthLabel(text: String) {
    Text(
        text = text,
        fontSize = 13.sp,
        fontWeight = FontWeight.Bold,
        color = Color.White,
    )
}

/**
 * The white-filled input treatment the front door's fields share: an 8dp radius, the dark teal
 * ink the reference puts on a white field, and the quiet border a flat input separates with. One
 * declaration rather than five copies of the same colour block, kept next to the fields it serves.
 */
@Composable
private fun authFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedContainerColor = Color.White,
    unfocusedContainerColor = Color.White,
    disabledContainerColor = Color.White.copy(alpha = 0.85f),
    focusedTextColor = Color(0xFF0F4C5C),
    unfocusedTextColor = Color(0xFF0F4C5C),
    disabledTextColor = Color(0xFF0F4C5C).copy(alpha = 0.7f),
    focusedPlaceholderColor = Color(0xFF0F4C5C).copy(alpha = 0.55f),
    unfocusedPlaceholderColor = Color(0xFF0F4C5C).copy(alpha = 0.55f),
    disabledPlaceholderColor = Color(0xFF0F4C5C).copy(alpha = 0.45f),
    focusedLabelColor = Color(0xFF0F4C5C),
    unfocusedLabelColor = Color(0xFF0F4C5C).copy(alpha = 0.8f),
    disabledLabelColor = Color(0xFF0F4C5C).copy(alpha = 0.55f),
    focusedBorderColor = Color(0xFF0F4C5C).copy(alpha = 0.45f),
    unfocusedBorderColor = Color(0xFF0F4C5C).copy(alpha = 0.28f),
    disabledBorderColor = Color(0xFF0F4C5C).copy(alpha = 0.2f),
    cursorColor = Color(0xFF0F4C5C),
)

/**
 * The "Server" disclosure, mirroring `clients/web/src/components/server-form.tsx`.
 *
 * A user who has never opened it sees a single summary line ("localhost:18080", say),
 * a chevron, and the always-visible transport picker; opening it reveals the structured
 * fields. The form's working state stays local until "Use this server" is clicked -- so
 * a half-typed entry is not pushed into the bootstrap, and a failed sign-in does not
 * have to re-fetch a not-yet-confirmed host. The transport is the exception: one tap on
 * the picker commits the swap immediately, because changing transport never needs the
 * host and port re-confirmed.
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
            colors = TextButtonDefaults.textButtonColors(
                contentColor = Color.White,
                disabledContentColor = Color.White.copy(alpha = 0.6f),
            ),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(text = if (open) "▾" else "▸")
                Spacer(modifier = Modifier.width(6.dp))
                Text(text = "Server")
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = serverSummary(host, port, gatewayPort, transport),
                    style = MaterialTheme.typography.bodySmall,
                    color = Color.White.copy(alpha = 0.82f),
                )
            }
        }
        // The transport is the one server choice that is not behind the disclosure: the picker
        // rides directly under the toggle and one tap commits the swap. A transport change never
        // needs the host and port re-confirmed, so it never lives in the draft.
        Text(
            text = "Transport",
            style = MaterialTheme.typography.labelMedium,
            color = Color.White.copy(alpha = 0.82f),
        )
        TransportChoice(
            value = value.transport,
            enabled = enabled,
            onChange = { chosen ->
                if (chosen != value.transport) {
                    // Swap the transport on the committed record, pairing the schemes the same
                    // way the web form's pickTransport does: QUIC picks QUIC/QUIC-TLS by the
                    // loopback rule, WebSocket restores the host's WS/WSS pair. The draft state
                    // below re-initialises from the new value, so the open panel follows.
                    val loopback = ServerEndpoint.isLoopbackHost(value.host)
                    val next = when (chosen) {
                        Transport.Tcp -> ServerEndpoint(
                            host = value.host,
                            port = value.port,
                            gatewayPort = value.gatewayPort,
                            transport = Transport.Tcp,
                            gatewayScheme = if (loopback) GatewayScheme.Tcp else GatewayScheme.TcpTls,
                            restScheme = if (loopback) RestScheme.Http else RestScheme.Https,
                        )
                        Transport.Quic -> ServerEndpoint(
                            host = value.host,
                            port = value.port,
                            gatewayPort = value.gatewayPort,
                            transport = Transport.Quic,
                            gatewayScheme = if (loopback) GatewayScheme.Quic else GatewayScheme.QuicTls,
                            restScheme = if (loopback) RestScheme.Http else RestScheme.Https,
                        )
                        Transport.WebSocket -> {
                            // The WebSocket family's own pair: WS for a loopback, WSS otherwise.
                            // defaultSchemesForHost returns the *native* pair for a loopback now,
                            // so reusing it here would hand the constructor a TCP scheme under a
                            // WebSocket transport and be rejected.
                            ServerEndpoint(
                                host = value.host,
                                port = value.port,
                                gatewayPort = value.gatewayPort,
                                transport = Transport.WebSocket,
                                gatewayScheme = if (loopback) GatewayScheme.Ws else GatewayScheme.Wss,
                                restScheme = if (loopback) RestScheme.Http else RestScheme.Https,
                            )
                        }
                    }
                    onCommit(next)
                }
            },
        )
        if (value.transport == Transport.Tcp) {
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = "TCP is the native default: one socket, one session, binary " +
                    "length-prefixed frames. If the server does not offer the TCP listener, " +
                    "this client falls back to WebSocket and says so.",
                style = MaterialTheme.typography.labelSmall,
                color = Color.White.copy(alpha = 0.82f),
            )
        }
        if (value.transport == Transport.Quic) {
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = "QUIC is a second option; it needs a server with the QUIC listener " +
                    "enabled. This build still connects over WebSocket.",
                style = MaterialTheme.typography.labelSmall,
                color = Color.White.copy(alpha = 0.82f),
            )
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
                    shape = RoundedCornerShape(8.dp),
                    colors = authFieldColors(),
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
                        shape = RoundedCornerShape(8.dp),
                        colors = authFieldColors(),
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
                        shape = RoundedCornerShape(8.dp),
                        colors = authFieldColors(),
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Number,
                            imeAction = ImeAction.Next,
                        ),
                        modifier = Modifier.weight(1f),
                    )
                }
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "Scheme",
                    style = MaterialTheme.typography.labelMedium,
                    color = Color.White.copy(alpha = 0.82f),
                )
                SchemeChoice(
                    transport = transport,
                    gatewayScheme = gatewayScheme,
                    restScheme = restScheme,
                    enabled = enabled,
                    onGateway = { gatewayScheme = it },
                    onRest = { restScheme = it },
                )
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
                        colors = TextButtonDefaults.textButtonColors(
                            contentColor = Color.White,
                            disabledContentColor = Color.White.copy(alpha = 0.6f),
                        ),
                    ) {
                        Text("Use this server")
                    }
                }
            }
        }
    }
}

/** The single-line summary shown when the disclosure is collapsed. */
private fun serverSummary(
    host: String,
    port: String,
    gatewayPort: String,
    transport: Transport,
): String {
    val h = host.ifBlank { "unset" }
    val p = port.ifBlank { "?" }
    val g = if (gatewayPort.isBlank()) "auto" else gatewayPort
    val t = when (transport) {
        Transport.Tcp -> "TCP"
        Transport.Quic -> "QUIC"
        Transport.WebSocket -> "WebSocket"
    }
    return "$h:$p  ·  gateway $g  ·  $t"
}

/**
 * The transport picker: TCP (the native default), WebSocket (the web client's transport), and QUIC
 * (the second option). Mirrors the web and desktop forms, so a user who picked "TCP" on the desktop
 * and picks it on Android sees the same choice. All are selectable and the choice persists; the
 * inline notes in [ServerDisclosure] are honest about what each one needs.
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
                    onClick = { if (enabled) onChange(option) },
                    enabled = enabled,
                )
                Text(
                    text = transportLabel(option),
                    style = MaterialTheme.typography.bodyMedium,
                    color = Color.White.copy(alpha = 0.9f),
                )
            }
        }
    }
}

/** The transport radio label, matching the web/desktop wording. */
private fun transportLabel(transport: Transport): String = when (transport) {
    Transport.Tcp -> "TCP (native default)"
    Transport.WebSocket -> "WebSocket"
    Transport.Quic -> "QUIC (second option)"
}

/**
 * The scheme picker: HTTP for loopback, HTTPS otherwise. The gateway scheme is paired with the
 * REST one within the chosen transport's family -- plain HTTP implies plain WS (or plain QUIC),
 * HTTPS implies WSS (or QUIC-TLS) -- and the gateway radios show WS/WSS under WebSocket and
 * QUIC/QUIC-TLS under QUIC. The picker lets a user override either side when the deployment is
 * split (TLS terminator in front of plain migod, say), at the cost of seeing a result that the
 * dev policy would have rejected as a default.
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
                            // Keep the gateway scheme paired with the REST posture, within the
                            // chosen transport's family: HTTPS -> the TLS side, HTTP -> the plain
                            // side (WS/WSS for WebSocket, QUIC/QUIC-TLS for QUIC).
                            onGateway(
                                when (transport) {
                                    Transport.Tcp ->
                                        if (option == RestScheme.Https) GatewayScheme.TcpTls else GatewayScheme.Tcp
                                    Transport.WebSocket ->
                                        if (option == RestScheme.Https) GatewayScheme.Wss else GatewayScheme.Ws
                                    Transport.Quic ->
                                        if (option == RestScheme.Https) GatewayScheme.QuicTls else GatewayScheme.Quic
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
        // Gateway scheme, kept paired but exposed for the reverse-proxy case. The options follow
        // the transport: TCP/TCP-TLS under TCP, WS/WSS under WebSocket, QUIC/QUIC-TLS under QUIC
        // (both spelled `quic` in the URL; the TLS posture rides in ALPN).
        val gatewayOptions = when (transport) {
            Transport.Tcp -> listOf(GatewayScheme.Tcp, GatewayScheme.TcpTls)
            Transport.WebSocket -> listOf(GatewayScheme.Ws, GatewayScheme.Wss)
            Transport.Quic -> listOf(GatewayScheme.Quic, GatewayScheme.QuicTls)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            gatewayOptions.forEach { option ->
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
                        text = gatewaySchemeLabel(option),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        }
    }
}

/** The gateway-scheme radio label, matching the web/desktop wording. */
private fun gatewaySchemeLabel(scheme: GatewayScheme): String = when (scheme) {
    GatewayScheme.Tcp -> "TCP (plain, dev-only)"
    GatewayScheme.TcpTls -> "TCP-TLS"
    GatewayScheme.Ws -> "WS"
    GatewayScheme.Wss -> "WSS"
    GatewayScheme.Quic -> "QUIC (plain)"
    GatewayScheme.QuicTls -> "QUIC-TLS"
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
    // The transport/scheme pairing is carried by the draft (the picker keeps QUIC on a QUIC
    // scheme and WebSocket on a WS scheme); the constructor validates it and throws a message
    // the disclosure shows verbatim if a pair somehow slipped through inconsistent.
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
