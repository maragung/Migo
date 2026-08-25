package com.migo.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.migo.core.protocol.PROTOCOL_NAME
import com.migo.core.protocol.PROTOCOL_VERSION

/**
 * The single Activity. Compose owns everything inside the window; this class only sets the
 * content and applies the app theme.
 *
 * The real screens (conversation list, chat, calls) mount here as the SDK's client,
 * transport, and storage layers land. For now it renders a placeholder that reads two
 * constants straight out of the generated protocol layer — enough to prove the :app module
 * links against :core and compiles into a runnable APK that CI assembles and a reviewer can
 * sideload.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MigoTheme {
                Scaffold(modifier = Modifier.fillMaxSize()) { padding ->
                    Column(
                        modifier = Modifier.fillMaxSize().padding(padding).padding(24.dp),
                        verticalArrangement = Arrangement.Center,
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(text = "Migo", style = MaterialTheme.typography.headlineLarge)
                        Text(
                            text = "$PROTOCOL_NAME/$PROTOCOL_VERSION",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
            }
        }
    }
}

/**
 * The app's Material 3 theme. Deliberately thin: it takes the Material baseline color
 * scheme rather than a bespoke palette, so there is nothing to maintain here until a design
 * system exists. It is a Composable wrapper, not the XML window theme in themes.xml — that
 * one only paints the window before Compose draws its first frame.
 */
@Composable
fun MigoTheme(content: @Composable () -> Unit) {
    MaterialTheme(content = content)
}
