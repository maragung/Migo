package com.migo.app.ui

import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState

/**
 * The sign-in and registration form, which are one screen because they differ by one button.
 *
 * Two forms would mean two copies of the server field, the account field and the password field, kept
 * consistent by hand, to express a difference the server treats as two endpoints and a person treats
 * as "I have an account" or "I do not".
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
    onServerUrl: (String) -> Unit,
    onIdentifier: (String) -> Unit,
    onSubmit: (password: String, create: Boolean) -> Unit,
    onDismissFailure: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var password by remember { mutableStateOf("") }
    // Whether the person is registering does survive a rotation: it is a choice they made, and not a
    // credential.
    var creating by rememberSaveable { mutableStateOf(false) }

    Column(modifier = modifier.fillMaxSize()) {
        ErrorBanner(message = form.failure, onDismiss = onDismissFailure)
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .imePadding()
                .padding(horizontal = 24.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(
                text = "Migo",
                style = MaterialTheme.typography.displaySmall,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.primary,
            )
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = "End-to-end encrypted. Your keys stay on this device.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            Spacer(modifier = Modifier.height(32.dp))

            OutlinedTextField(
                value = form.serverUrl,
                onValueChange = onServerUrl,
                label = { Text("Server") },
                placeholder = { Text("http://10.0.2.2:8080") },
                singleLine = true,
                enabled = !form.busy,
                keyboardOptions = KeyboardOptions(
                    keyboardType = KeyboardType.Uri,
                    imeAction = ImeAction.Next,
                ),
                modifier = Modifier.fillMaxWidth(),
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
                modifier = Modifier.fillMaxWidth().height(52.dp),
            ) {
                if (form.busy) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(18.dp),
                            strokeWidth = 2.dp,
                            color = MaterialTheme.colorScheme.onPrimary,
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
            Spacer(modifier = Modifier.height(16.dp))

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
