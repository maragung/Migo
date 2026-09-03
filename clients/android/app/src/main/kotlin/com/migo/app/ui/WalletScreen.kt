package com.migo.app.ui

import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.ChainNetworkChoice
import com.migo.app.model.ChainState
import com.migo.app.model.ChainTxRow
import com.migo.app.model.PreparedChainTx
import com.migo.app.model.avaxOf
import com.migo.app.model.navaxOf
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
    onChainNetwork: (ChainNetworkChoice) -> Unit,
    onChainBalance: () -> Unit,
    onChainPrepare: (recipient: String, amount: String) -> Unit,
    onChainAcknowledged: (Boolean) -> Unit,
    onChainCancel: () -> Unit,
    onChainSend: (PreparedChainTx) -> Unit,
    modifier: Modifier = Modifier,
) {
    // The picker survives recomposition but not process death: a gift half-addressed is cheaply
    // re-chosen, and GiftListing is not a saveable type.
    var picking: GiftListing? by remember { mutableStateOf<GiftListing?>(null) }
    var recipientField by rememberSaveable { mutableStateOf("") }
    // The AVAX send form's own visibility; its text survives rotation in saveables below.
    var chainSending by rememberSaveable { mutableStateOf(false) }

    val kindFriend: Long = RelationshipKind.Friend.wire.toLong()
    val friends = state.friends.entries.filter { it.kind == kindFriend }

    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Wallet") {
            TextButton(onClick = onRefresh, enabled = !state.wallet.loading) { Text("Refresh") }
        }

        if (state.wallet.loading && state.wallet.balance == null) {
            LoadingRow()
        } else {
            // The list is weighted, not fillMaxSize: a Column measures its non-weighted children
            // first (the gift and AVAX forms below) and gives the weighted one what remains — the
            // other order measures an unweighted fillMaxSize list at the whole remaining height
            // and leaves the forms below it nothing, which is how both send flows once became
            // forms no phone could see.
            LazyColumn(modifier = Modifier.weight(1f)) {
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

                // The AVAX side (§184): one named network at a time, balance by explicit refresh.
                item {
                    ChainPanel(
                        chain = state.wallet.chain,
                        onNetwork = onChainNetwork,
                        onBalance = onChainBalance,
                        onSend = { chainSending = true },
                    )
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
                        // Badges are honours, not layout: the row scrolls rather than wrapping
                        // (a second row would push the gift shop's prices down mid-read) or
                        // clipping the honours a long account has earned.
                        Row(
                            modifier = Modifier.fillMaxWidth()
                                .horizontalScroll(rememberScrollState())
                                .padding(horizontal = 16.dp),
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

                // The AVAX activity: the account's own tracked sends, newest first.
                if (state.wallet.chain.activity.isNotEmpty()) {
                    item { SectionLabel(text = "AVAX activity") }
                    items(state.wallet.chain.activity, key = { it.txHash }) { row ->
                        ChainTxLine(row = row)
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
        // The AVAX send form: one screen, form then full transaction, mirroring the desktop
        // client's send window. It closes itself the moment a broadcast is accepted -- what
        // follows is the tracking line's business, not the form's.
        LaunchedEffect(state.wallet.chain.tracking) {
            if (state.wallet.chain.tracking != null) chainSending = false
        }
        if (chainSending) {
            ChainSendForm(
                chain = state.wallet.chain,
                onPrepare = onChainPrepare,
                onAcknowledged = onChainAcknowledged,
                onCancel = {
                    chainSending = false
                    onChainCancel()
                },
                onSend = onChainSend,
            )
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

/**
 * The AVAX panel (§184): one named network at a time, wallet 0's address, a balance the user asks
 * for, and the way out to the send form.
 *
 * The network is two names and no URL — a self-supplied RPC is the classic way a wallet gets shown
 * a fake chain. The balance is a pull, never a poll, and an error stays on screen because "could
 * not check" and "zero" are different facts.
 */
@Composable
private fun ChainPanel(
    chain: ChainState,
    onNetwork: (ChainNetworkChoice) -> Unit,
    onBalance: () -> Unit,
    onSend: () -> Unit,
) {
    val clipboard = LocalClipboardManager.current
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        SectionLabel(text = "AVAX")
        Row(
            horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp),
        ) {
            FilterChip(
                selected = chain.network == ChainNetworkChoice.MAINNET,
                onClick = { onNetwork(ChainNetworkChoice.MAINNET) },
                label = { Text("Mainnet") },
            )
            FilterChip(
                selected = chain.network == ChainNetworkChoice.FUJI,
                onClick = { onNetwork(ChainNetworkChoice.FUJI) },
                label = { Text("Fuji (testnet)") },
            )
        }
        Spacer(modifier = Modifier.height(8.dp))
        // The address, EIP-55: the form a person can check a character of, and a copy button
        // because nobody retypes forty-two characters without introducing a typo.
        chain.address?.let { address ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = address,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = { clipboard.setText(AnnotatedString(address)) }) { Text("Copy") }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = chain.balance?.let { avaxOf(it) + " AVAX" } ?: "balance after a refresh",
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onBalance) { Text("Refresh") }
        }
        chain.error?.let { error ->
            Text(text = error, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        }
        chain.tracking?.let { tracking ->
            Text(
                text = "tracking " + tracking.txHash.take(14) + "… · " + tracking.state,
                style = MaterialTheme.typography.bodySmall,
                color = LocalMigoExtra.current.faint,
            )
        }
        OutlinedButton(onClick = onSend, enabled = chain.tracking == null, modifier = Modifier.fillMaxWidth()) {
            Text("Send AVAX")
        }
    }
}

/**
 * The AVAX send form: one screen, form then full transaction.
 *
 * The confirm half shows every field before anything is signed, and the confirm button hands the
 * prepared struct back verbatim — the view model re-parses and re-checks it, so what is signed is
 * what was shown (spec #40). Mainnet is real money, and the first send on it says so before the
 * button unlocks.
 */
@Composable
private fun ChainSendForm(
    chain: ChainState,
    onPrepare: (recipient: String, amount: String) -> Unit,
    onAcknowledged: (Boolean) -> Unit,
    onCancel: () -> Unit,
    onSend: (PreparedChainTx) -> Unit,
) {
    var recipient by rememberSaveable { mutableStateOf("") }
    var amount by rememberSaveable { mutableStateOf("") }
    val prepared = chain.prepared
    Column(modifier = Modifier.fillMaxWidth().padding(16.dp).imePadding()) {
        Surface(
            color = MaterialTheme.colorScheme.surfaceVariant,
            shape = MaterialTheme.shapes.medium,
        ) {
            Column(modifier = Modifier.padding(12.dp)) {
                if (prepared == null) {
                    Text(
                        text = "Send AVAX · " + chain.network.label,
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Spacer(modifier = Modifier.height(8.dp))
                    OutlinedTextField(
                        value = recipient,
                        onValueChange = { recipient = it },
                        label = { Text("Recipient address (0x…)") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Spacer(modifier = Modifier.height(8.dp))
                    OutlinedTextField(
                        value = amount,
                        onValueChange = { amount = it },
                        label = { Text("Amount (AVAX)") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    chain.prepareError?.let { error ->
                        Spacer(modifier = Modifier.height(8.dp))
                        Text(
                            text = error,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    Spacer(modifier = Modifier.height(8.dp))
                    Row {
                        TextButton(onClick = onCancel) { Text("Cancel") }
                        Spacer(modifier = Modifier.width(8.dp))
                        Button(
                            onClick = { onPrepare(recipient, amount) },
                            enabled = recipient.isNotBlank() && amount.isNotBlank(),
                        ) { Text("Build") }
                    }
                } else {
                    Text(text = "Confirm the transaction", style = MaterialTheme.typography.titleMedium)
                    Spacer(modifier = Modifier.height(8.dp))
                    PreparedLine(label = "From", value = prepared.from)
                    PreparedLine(label = "To", value = prepared.to)
                    PreparedLine(label = "Amount", value = avaxOf(prepared.valueWei) + " AVAX")
                    PreparedLine(
                        label = "Max fee",
                        value = navaxOf(
                            prepared.maxFeePerGas.multiply(java.math.BigInteger.valueOf(prepared.gasLimit)),
                        ) + " nAVAX",
                    )
                    PreparedLine(
                        label = "Max priority fee",
                        value = navaxOf(prepared.maxPriorityFeePerGas) + " nAVAX",
                    )
                    PreparedLine(label = "Gas limit", value = prepared.gasLimit.toString())
                    PreparedLine(label = "Nonce", value = prepared.nonce.toString())
                    PreparedLine(label = "Chain", value = prepared.network.label)
                    if (prepared.network == ChainNetworkChoice.MAINNET) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(
                                checked = chain.mainnetAcknowledged,
                                onCheckedChange = onAcknowledged,
                            )
                            Text(
                                text = "This is mainnet AVAX — real money, sent to the address above, not reversible.",
                                style = MaterialTheme.typography.bodySmall,
                                modifier = Modifier.weight(1f),
                            )
                        }
                    }
                    chain.sendError?.let { error ->
                        Spacer(modifier = Modifier.height(8.dp))
                        Text(
                            text = error,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    Spacer(modifier = Modifier.height(8.dp))
                    Row {
                        TextButton(onClick = onCancel) { Text("Back") }
                        Spacer(modifier = Modifier.width(8.dp))
                        Button(
                            onClick = { onSend(prepared) },
                            enabled = chain.tracking == null &&
                                (prepared.network != ChainNetworkChoice.MAINNET || chain.mainnetAcknowledged),
                        ) { Text("Confirm send") }
                    }
                }
            }
        }
    }
}

/** One line of the confirm screen: a label, and the exact value that will be signed. */
@Composable
private fun PreparedLine(label: String, value: String) {
    Column(modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
        )
        Text(
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = FontFamily.Monospace,
        )
    }
}

/**
 * One tracked AVAX send as the Activity list draws it.
 *
 * The fee reads as a ceiling until the receipt replaces it with the gas actually spent — a
 * confirmed spend should never overstate what it cost. The hash is the explorer's handle:
 * shortened here, whole in the clipboard copy.
 */
@Composable
private fun ChainTxLine(row: ChainTxRow) {
    val clipboard = LocalClipboardManager.current
    val extra = LocalMigoExtra.current
    val (word, tone) = when (row.outcome) {
        "CONFIRMED" -> "confirmed" to MaterialTheme.colorScheme.secondary
        "REVERTED", "DROPPED" -> row.outcome.lowercase() to MaterialTheme.colorScheme.error
        "EXPIRED" -> "expired" to extra.gold
        else -> row.outcome to extra.faint
    }
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "-" + avaxOf(row.valueWei) + " AVAX",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = word,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.Bold,
                color = tone,
            )
        }
        OneLine(text = "to " + row.to.take(10) + "… · " + row.network)
        val fee = if (row.gasUsed != null && row.block != null) {
            "fee " + row.gasUsed + " gas"
        } else {
            "fee ≤ " + navaxOf(row.feeWei) + " nAVAX"
        }
        OneLine(
            text = listOfNotNull(fee, row.block?.let { "block $it" }, clockTime(row.at)).joinToString(" · "),
        )
        TextButton(onClick = { clipboard.setText(AnnotatedString(row.txHash)) }) {
            Text(text = row.txHash.take(14) + "…", style = MaterialTheme.typography.labelSmall)
        }
    }
}
