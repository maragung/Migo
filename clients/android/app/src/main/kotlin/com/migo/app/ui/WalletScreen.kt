package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.core.protocol.GiftListing
import com.migo.core.protocol.LedgerEntryWire
import com.migo.core.protocol.RelationshipKind
import com.migo.core.wire.Id

/**
 * The Wallet section: the MIG balance, the gift shop, the statement, progression, badges, and the
 * leaderboard — the caller's whole economy under one address.
 *
 * The coin is MIG. The balance leads; the statement states each line's signed amount from its reason
 * (the wire's amount is a magnitude); the shop states its prices before its recipients, so the spend
 * is agreed before the address is.
 */
@Composable
fun WalletScreen(
    state: AppState.SignedIn,
    onSendGift: (sku: String, recipient: Id) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // The picker survives recomposition but not process death: a gift half-addressed is cheaply
    // re-chosen, and GiftListing is not a saveable type.
    var picking: GiftListing? by androidx.compose.runtime.remember { mutableStateOf<GiftListing?>(null) }
    var recipientField by rememberSaveable { mutableStateOf("") }

    val kindFriend: Long = RelationshipKind.Friend.wire.toLong()
    val friends = state.friends.entries.filter { it.kind == kindFriend }

    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Wallet") {
            TextButton(onClick = onRefresh, enabled = !state.wallet.loading) { Text("Refresh") }
        }

        if (state.wallet.loading && state.wallet.balance == null) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(24.dp),
                horizontalArrangement = androidx.compose.foundation.layout.Arrangement.Center,
            ) { CircularProgressIndicator() }
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                // The balance: the two facts, coins first.
                item {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp),
                    ) {
                        BalanceFact(
                            amount = state.wallet.balance?.toString() ?: "…",
                            unit = "MIG coins",
                            emphasise = true,
                        )
                        BalanceFact(
                            amount = state.wallet.points?.toString() ?: "…",
                            unit = "points",
                            emphasise = false,
                        )
                    }
                }

                // Progression: level and the bar behind it.
                state.wallet.progression?.let { progression ->
                    item {
                        Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
                            SectionLabel(text = "Level " + progression.level)
                            LinearProgressIndicator(
                                progress = { xpFraction(progression.xpIntoLevel, progression.xpForNextLevel) },
                                modifier = Modifier.fillMaxWidth(),
                            )
                            Text(
                                text = "${progression.xpIntoLevel} / ${progression.xpForNextLevel} XP",
                                style = MaterialTheme.typography.labelSmall,
                                color = LocalMigoExtra.current.faint,
                            )
                        }
                    }
                }

                // Badges: the honours, one chip each.
                if (state.wallet.badges.isNotEmpty()) {
                    item { SectionLabel(text = "Badges") }
                    item {
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                            horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp),
                        ) {
                            for (badge in state.wallet.badges) {
                                Surface(
                                    color = MaterialTheme.colorScheme.tertiaryContainer,
                                    contentColor = MaterialTheme.colorScheme.onTertiaryContainer,
                                    shape = MaterialTheme.shapes.large,
                                ) {
                                    Text(
                                        text = badge.badgeCode.replace('_', ' ').replaceFirstChar { it.uppercase() },
                                        style = MaterialTheme.typography.labelMedium,
                                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                                    )
                                }
                            }
                        }
                    }
                }

                // The gift shop: price stated before recipient, per the panel's own rule.
                if (state.wallet.catalogue.isNotEmpty()) {
                    item { SectionLabel(text = "Send a gift") }
                    items(state.wallet.catalogue, key = { it.sku }) { gift ->
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(gift.name, style = MaterialTheme.typography.titleMedium)
                                OneLine(text = "${gift.price} MIG · ${gift.category}")
                            }
                            Button(onClick = { picking = gift }) { Text("Send") }
                        }
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }

                // The statement: one line per transaction, newest first as the server ordered them.
                if (state.wallet.ledger.isNotEmpty()) {
                    item { SectionLabel(text = "Recent activity") }
                    items(state.wallet.ledger, key = { it.txId.value }) { entry ->
                        LedgerLine(entry = entry)
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }

                // The leaderboard.
                if (state.wallet.leaders.isNotEmpty()) {
                    item { SectionLabel(text = "Leaderboard") }
                    items(state.wallet.leaders, key = { it.accountId.value }) { rank ->
                        ActivityLine(title = "#${rank.position}  Level ${rank.level}  ${rank.xp} XP", at = null)
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }

                item { Spacer(modifier = Modifier.height(16.dp)) }
            }
        }

        // The recipient flow: the picked gift, its price restated, and the address.
        picking?.let { gift ->
            Column(modifier = Modifier.fillMaxWidth().padding(16.dp).imePadding()) {
                Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = MaterialTheme.shapes.medium,
                ) {
                    Column(modifier = Modifier.padding(12.dp)) {
                        Text(
                            text = "Send " + gift.name + " (" + gift.price + " MIG)",
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Spacer(modifier = Modifier.height(8.dp))
                        OutlinedTextField(
                            value = recipientField,
                            onValueChange = { recipientField = it },
                            placeholder = { Text("Account id or friend") },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        if (friends.isNotEmpty()) {
                            Spacer(modifier = Modifier.height(8.dp))
                            for (friend in friends.take(3)) {
                                TextButton(onClick = { recipientField = friend.userId.value }) {
                                    Text("Friend " + friend.userId.value.take(8))
                                }
                            }
                        }
                        Spacer(modifier = Modifier.height(8.dp))
                        Row {
                            TextButton(onClick = {
                                picking = null
                                recipientField = ""
                            }) { Text("Cancel") }
                            Spacer(modifier = Modifier.width(8.dp))
                            Button(
                                onClick = {
                                    val id = try {
                                        com.migo.core.wire.parseId(recipientField.trim())
                                    } catch (_: Exception) {
                                        null
                                    }
                                    if (id != null) {
                                        onSendGift(gift.sku, id)
                                        picking = null
                                        recipientField = ""
                                    }
                                },
                                enabled = recipientField.isNotBlank(),
                            ) { Text("Send") }
                        }
                    }
                }
            }
        }
    }
}

/** One of the two balance facts, drawn compactly. A RowScope member so it can weigh itself. */
@Composable
private fun androidx.compose.foundation.layout.RowScope.BalanceFact(
    amount: String,
    unit: String,
    emphasise: Boolean,
) {
    Surface(
        color = if (emphasise) {
            MaterialTheme.colorScheme.primaryContainer
        } else {
            MaterialTheme.colorScheme.surfaceVariant
        },
        contentColor = if (emphasise) {
            MaterialTheme.colorScheme.onPrimaryContainer
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.weight(1f),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = amount,
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
            )
            Text(
                text = unit,
                style = MaterialTheme.typography.labelMedium,
            )
        }
    }
}

/** One statement line: reason, signed amount, the balance after, and when. */
@Composable
private fun LedgerLine(entry: LedgerEntryWire) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = entry.reason.replace('_', ' ').replaceFirstChar { it.uppercase() },
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = "balance " + entry.balanceAfter,
                style = MaterialTheme.typography.labelSmall,
                color = LocalMigoExtra.current.faint,
            )
        }
        Text(
            text = ledgerAmount(entry),
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            color = if (ledgerCredits(entry.reason)) {
                MaterialTheme.colorScheme.secondary
            } else {
                MaterialTheme.colorScheme.onSurface
            },
        )
    }
}

/** The closed reason-to-direction mapping, identical to the web client's. */
fun ledgerCredits(reason: String): Boolean =
    reason == "grant" || reason == "gift_reputation" || reason == "refund" || reason == "game_payout"

/** The signed amount a statement line shows, from the reason's direction. */
fun ledgerAmount(entry: LedgerEntryWire): String =
    (if (ledgerCredits(entry.reason)) "+" else "-") + entry.amount

/** The XP bar's filled fraction, clamped into 0..1 — an unfilled bar is honest, NaN% is not. */
fun xpFraction(into: Long, total: Long): Float =
    if (total <= 0L) 0f else (into.toFloat() / total.toFloat()).coerceIn(0f, 1f)
