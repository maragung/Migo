'use client';

/**
 * The AVAX side of the Wallet section (§184): wallet 0's address and balance on one named network,
 * and the send flow that shows every field before anything is signed.
 *
 * The chain conversation is this browser's own: `ChainClient` talks to Avalanche's public C-Chain
 * RPC and skips the Migo server entirely — the server is never a blockchain proxy and never sees a
 * transaction. The network is picked from two names, never a URL, and the balance is a pull, never
 * a poll: an error stays on screen because "could not check" and "zero" are different facts.
 *
 * The send flow is the desktop's, translated: parse failures happen before a single RPC leaves,
 * the confirm step quotes every field the signature will cover, the mainnet acknowledgement says
 * what mainnet means before the button that spends unlocks, and acceptance is never confirmation —
 * BROADCAST is what `eth_sendRawTransaction` returning a hash means, and CONFIRMED arrives only
 * from a receipt's `status: 1` (spec #41). The record is written at broadcast, not at settle, so a
 * reload mid-tracking loses the ending, never the fact that value left.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { account, AVALANCHE_MAINNET, ChainClient, FUJI_TESTNET } from '@migo/sdk';
import type { MigoClient, Network, TrackedTx, TrackedOutcome } from '@migo/sdk';

import { avaxOf, hexOf, navaxOf, parseAvaxAmount } from '@/lib/avax.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

/** The two first-class networks, as the chips name them. */
type NetworkChoice = 'mainnet' | 'fuji';

interface PreparedTx {
  network: NetworkChoice;
  chainId: number;
  from: string;
  to: string;
  valueWei: bigint;
  maxPriorityFeePerGas: bigint;
  maxFeePerGas: bigint;
  gasLimit: number;
  nonce: number;
}

/** What a device without the root is told, in one sentence, wherever the AVAX wallet is asked for. */
const NO_ROOT_ON_DEVICE =
  'This device does not hold the account root, so it has no AVAX wallet; open the wallet on the device that holds the account backup.';

/** The two names as the pinned `Network` constants each carries. */
function networkOf(choice: NetworkChoice): Network {
  return choice === 'mainnet' ? AVALANCHE_MAINNET : FUJI_TESTNET;
}

/** What a malformed recipient is told, wherever the address is parsed. */
const BAD_ADDRESS = 'The recipient is not a valid address; paste the full 0x… address';

/** The chip label, the same two words every client offers. */
function networkLabel(choice: NetworkChoice): string {
  return choice === 'mainnet' ? 'Avalanche C-Chain (mainnet)' : 'Avalanche Fuji (testnet)';
}

/** A chain id as its network's name; one this build cannot name labels itself honestly. */
export function networkName(chainId: number): string {
  if (chainId === AVALANCHE_MAINNET.chainId) {
    return AVALANCHE_MAINNET.name;
  }
  if (chainId === FUJI_TESTNET.chainId) {
    return FUJI_TESTNET.name;
  }
  return `chain ${chainId}`;
}

/**
 * The fee a tracked send shows: the gas the receipt says the block actually spent once there is a
 * receipt, and the confirmed ceiling until then — a confirmed spend should never overstate what it
 * cost, and an unconfirmed one should never understate it.
 */
export function feeLabel(row: { gasUsed?: bigint; block?: number; feeWei: bigint }): string {
  if (row.gasUsed !== undefined && row.block !== undefined) {
    return `fee ${row.gasUsed} gas`;
  }
  return `fee ≤ ${navaxOf(row.feeWei)} nAVAX`;
}

/** Spec #41's own word in the tone the outcome earns. */
function outcomeClass(outcome: string): string {
  if (outcome === 'CONFIRMED') {
    return 'avax-outcome avax-outcome-confirmed';
  }
  if (outcome === 'REVERTED' || outcome === 'DROPPED') {
    return 'avax-outcome avax-outcome-failed';
  }
  if (outcome === 'EXPIRED') {
    return 'avax-outcome avax-outcome-expired';
  }
  return 'avax-outcome';
}

/** One tracked send as the Activity list draws it. */
export function ChainTxLine({ row }: { row: TrackedTx }): ReactNode {
  const txHash = `0x${hexOf(row.txHash)}`;
  return (
    <li className="ledger-row avax-row">
      <span className="ledger-reason">
        −{avaxOf(row.valueWei)} AVAX to {`0x${hexOf(row.to)}`.slice(0, 10)}…
      </span>
      <span className="ledger-amount">{feeLabel(row)}</span>
      <span className="avax-network">{networkName(row.chainId)}</span>
      <span className={outcomeClass(row.outcome)}>{row.outcome.toLowerCase()}</span>
      <button
        type="button"
        className="avax-hash"
        title="Copy the transaction hash"
        onClick={() => {
          void navigator.clipboard.writeText(txHash).catch(() => {});
        }}
      >
        {txHash.slice(0, 14)}…
      </button>
    </li>
  );
}

/** One line of the confirm screen: a label, and the exact value that will be signed. */
function PreparedLine({ label, value }: { label: string; value: string }): ReactNode {
  return (
    <div className="avax-prepared-line">
      <span className="avax-prepared-label">{label}</span>
      <span className="avax-prepared-value">{value}</span>
    </div>
  );
}

/**
 * The AVAX panel. Owns its own chain state the way the MIG side owns its reads: a refresh the user
 * asks for, a send flow with its own error lines, and a tracked list seeded from the key store's
 * sealed records.
 */
export function AvaxSection(): ReactNode {
  const { client, persistKeyStore } = useMigo();

  const [network, setNetwork] = useState<NetworkChoice>('mainnet');
  const [address, setAddress] = useState<string | null>(null);
  const [balance, setBalance] = useState<bigint | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [prepared, setPrepared] = useState<PreparedTx | null>(null);
  const [prepareError, setPrepareError] = useState<string | null>(null);
  const [sendError, setSendError] = useState<string | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [tracking, setTracking] = useState<{ txHash: string; state: string } | null>(null);
  const [activity, setActivity] = useState<TrackedTx[]>([]);
  const [sending, setSending] = useState(false);

  // An RPC in flight belongs to the network it was asked about. `network` in a closure goes stale
  // the moment the chips are tapped, so the ref is the truth an answer is checked against: an
  // answer that arrives after a switch is dropped, never shown next to the other network's name.
  const networkRef = useRef<NetworkChoice>('mainnet');

  /** Switches the network by name; everything the other network's RPC said is cleared. */
  function selectNetwork(choice: NetworkChoice): void {
    if (choice === networkRef.current) {
      return;
    }
    networkRef.current = choice;
    setNetwork(choice);
    setBalance(null);
    setError(null);
    setPrepared(null);
    setPrepareError(null);
    setSendError(null);
  }

  // The tracked list is the key store's own sealed record, read once here and re-read after every
  // mutation this flow makes; nothing else on the page changes it.
  useEffect(() => {
    if (client) {
      setActivity([...client.keyStore.trackedTxs()]);
    }
  }, [client]);

  /** Wallet 0's address and balance, on the network on screen. */
  const refreshBalance = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    const root = client.keyStore.root();
    if (root === null) {
      setAddress(null);
      setBalance(null);
      setError(NO_ROOT_ON_DEVICE);
      return;
    }
    const wallet = account.EvmWallet.fromRoot(root, 0);
    try {
      const next = await new ChainClient({ network: networkOf(network) }).getBalance(
        wallet.address(),
      );
      if (networkRef.current === network) {
        setAddress(wallet.addressChecksummed());
        setBalance(next);
        setError(null);
      }
    } catch (cause) {
      if (networkRef.current === network) {
        setAddress(wallet.addressChecksummed());
        setError(friendlyError(cause));
      }
    }
  }, [client, network]);

  /** Builds one transfer from the RPC's own answers; parse failures leave before any RPC does. */
  const prepare = useCallback(
    (recipient: string, amount: string): void => {
      if (!client) {
        return;
      }
      setPrepared(null);
      setPrepareError(null);
      let to: Uint8Array;
      try {
        to = account.parseAddress(recipient.trim());
      } catch {
        setPrepareError(BAD_ADDRESS);
        return;
      }
      const valueWei = parseAvaxAmount(amount);
      if (valueWei === null) {
        setPrepareError('The amount is not a valid AVAX amount, e.g. 1.5');
        return;
      }
      const root = client.keyStore.root();
      if (root === null) {
        setPrepareError(NO_ROOT_ON_DEVICE);
        return;
      }
      setSending(true);
      void (async (): Promise<void> => {
        try {
          const wallet = account.EvmWallet.fromRoot(root, 0);
          const chain = new ChainClient({ network: networkOf(network) });
          // The three lines the confirm screen quotes are the three reads: a prepared transaction
          // with a guessed field is a confirmation screen that lies about one of its lines.
          const [fees, gasLimit, nonce] = await Promise.all([
            chain.getFees(),
            chain.estimateGas({
              from: wallet.address(),
              to,
              value: valueWei,
              data: new Uint8Array(0),
            }),
            chain.getNonce(wallet.address()),
          ]);
          if (networkRef.current !== network) {
            return;
          }
          setPrepared({
            network,
            chainId: networkOf(network).chainId,
            from: wallet.addressChecksummed(),
            to: account.eip55(to),
            valueWei,
            maxPriorityFeePerGas: fees.maxPriorityFeePerGas,
            maxFeePerGas: fees.maxFeePerGas,
            gasLimit,
            nonce,
          });
        } catch (cause) {
          if (networkRef.current === network) {
            setPrepareError(friendlyError(cause));
          }
        } finally {
          setSending(false);
        }
      })();
    },
    [client, network],
  );

  /** Signs and broadcasts exactly the transaction the confirm screen displayed. */
  const confirmSend = useCallback(
    (tx: PreparedTx): void => {
      if (!client || tracking !== null) {
        return;
      }
      let to: Uint8Array;
      try {
        to = account.parseAddress(tx.to.trim());
      } catch {
        setSendError(BAD_ADDRESS);
        return;
      }
      const root = client.keyStore.root();
      if (root === null) {
        setSendError(NO_ROOT_ON_DEVICE);
        return;
      }
      const wallet = account.EvmWallet.fromRoot(root, 0);
      // The `from` on screen must be this device's wallet 0: a prepared transaction carried over
      // from another device, or an older derivation, is refused rather than signed with the wrong
      // key for the right-looking screen.
      if (tx.from !== wallet.addressChecksummed()) {
        setSendError('The prepared transaction names a different sender; prepare it again here');
        return;
      }
      const chainNetwork = networkOf(tx.network);
      const body = new account.Eip1559Tx({
        chainId: chainNetwork.chainId,
        nonce: tx.nonce,
        maxPriorityFeePerGas: tx.maxPriorityFeePerGas,
        maxFeePerGas: tx.maxFeePerGas,
        gasLimit: tx.gasLimit,
        to,
        value: tx.valueWei,
        data: new Uint8Array(0),
      });
      setSending(true);
      setSendError(null);
      void (async (): Promise<void> => {
        try {
          const signed = body.sign(wallet);
          const txHash = await new ChainClient({ network: chainNetwork }).broadcast(signed);
          // The record is written at broadcast, not at settle: a reload mid-tracking loses the
          // ending, never the fact that value left.
          client.keyStore.trackedTxs().unshift({
            txHash: signed.txHash(),
            chainId: chainNetwork.chainId,
            to,
            valueWei: tx.valueWei,
            feeWei: tx.maxFeePerGas * BigInt(tx.gasLimit),
            gasLimit: tx.gasLimit,
            atUnix: Math.floor(Date.now() / 1000),
            outcome: 'PENDING',
          });
          setActivity([...client.keyStore.trackedTxs()]);
          // Acceptance, not confirmation — the tracker below is the only thing that can say
          // CONFIRMED.
          setTracking({ txHash, state: 'BROADCAST' });
          setPrepared(null);
          setAcknowledged(false);
          persistKeyStore();

          // An endpoint that cannot be asked at all is still an unresolved ending, and EXPIRED is
          // the honest name for one this client watched for its whole deadline.
          let outcome: TrackedOutcome;
          let block: number | undefined;
          let gasUsed: bigint | undefined;
          try {
            const result = await new ChainClient({ network: chainNetwork }).track(txHash, {
              onState: (state) => {
                setTracking((current) =>
                  current !== null && current.txHash === txHash ? { txHash, state } : current,
                );
              },
            });
            outcome = result.outcome;
            block = result.blockNumber;
            gasUsed = result.gasUsed;
          } catch {
            outcome = 'EXPIRED';
          }
          settle(client, txHash, outcome, block, gasUsed);
          setTracking((current) =>
            current !== null && current.txHash === txHash ? null : current,
          );
          setActivity([...client.keyStore.trackedTxs()]);
          persistKeyStore();
        } catch (cause) {
          setSendError(friendlyError(cause));
        } finally {
          setSending(false);
        }
      })();
    },
    [client, tracking, persistKeyStore],
  );

  return (
    <section className="panel-section" aria-label="AVAX">
      <h2 className="panel-heading">AVAX</h2>
      <div className="chip-row" role="group" aria-label="Network">
        <button
          type="button"
          className={`chip ${network === 'mainnet' ? 'chip-active' : ''}`}
          onClick={() => selectNetwork('mainnet')}
        >
          Mainnet
        </button>
        <button
          type="button"
          className={`chip ${network === 'fuji' ? 'chip-active' : ''}`}
          onClick={() => selectNetwork('fuji')}
        >
          Fuji (testnet)
        </button>
      </div>

      {address !== null ? (
        <p className="avax-address">
          <code>{address}</code>
          <button
            type="button"
            className="btn"
            onClick={() => {
              void navigator.clipboard.writeText(address).catch(() => {});
            }}
          >
            Copy
          </button>
        </p>
      ) : null}

      <p className="avax-balance">
        <span className="avax-balance-amount">
          {balance !== null ? avaxOf(balance) : 'balance after a refresh'}
          {balance !== null ? ' AVAX' : ''}
        </span>
        <button type="button" className="btn" onClick={() => void refreshBalance()}>
          Refresh
        </button>
      </p>

      {error !== null ? <p className="form-error">{error}</p> : null}
      {tracking !== null ? (
        <p className="hint">
          tracking {tracking.txHash.slice(0, 14)}… · {tracking.state}
        </p>
      ) : null}

      {sending ? (
        <AvaxSendForm
          network={network}
          prepared={prepared}
          prepareError={prepareError}
          sendError={sendError}
          acknowledged={acknowledged}
          busy={tracking !== null}
          onPrepare={prepare}
          onAcknowledged={setAcknowledged}
          onCancel={() => {
            setSending(false);
            setPrepared(null);
            setPrepareError(null);
            setSendError(null);
            setAcknowledged(false);
          }}
          onSend={confirmSend}
        />
      ) : (
        <button
          type="button"
          className="btn btn-primary"
          disabled={tracking !== null}
          onClick={() => setSending(true)}
        >
          Send AVAX
        </button>
      )}

      {activity.length > 0 ? (
        <div className="panel-section">
          <h3 className="panel-heading">AVAX activity</h3>
          <ul className="ledger-list">
            {activity.map((row) => (
              <ChainTxLine key={hexOf(row.txHash)} row={row} />
            ))}
          </ul>
        </div>
      ) : null}
    </section>
  );
}

/** Writes a tracker's ending into the record the vault will next read. */
function settle(
  client: MigoClient,
  txHash: string,
  outcome: TrackedOutcome,
  block?: number,
  gasUsed?: bigint,
): void {
  const records = client.keyStore.trackedTxs();
  const index = records.findIndex((record) => `0x${hexOf(record.txHash)}` === txHash);
  const record = index >= 0 ? records[index] : undefined;
  if (record !== undefined) {
    records[index] = { ...record, outcome, block, gasUsed };
  }
}

/** The send flow: the form, then the full transaction, then the acknowledgement that unlocks send. */
function AvaxSendForm({
  network,
  prepared,
  prepareError,
  sendError,
  acknowledged,
  busy,
  onPrepare,
  onAcknowledged,
  onCancel,
  onSend,
}: {
  network: NetworkChoice;
  prepared: PreparedTx | null;
  prepareError: string | null;
  sendError: string | null;
  acknowledged: boolean;
  busy: boolean;
  onPrepare: (recipient: string, amount: string) => void;
  onAcknowledged: (value: boolean) => void;
  onCancel: () => void;
  onSend: (tx: PreparedTx) => void;
}): ReactNode {
  const [recipient, setRecipient] = useState('');
  const [amount, setAmount] = useState('');

  return (
    <div className="recipient-picker avax-send" role="dialog" aria-label="Send AVAX">
      {prepared === null ? (
        <>
          <div className="panel-head">
            <h2 className="panel-heading">Send AVAX</h2>
            <span className="avax-network">{networkLabel(network)}</span>
          </div>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              onPrepare(recipient, amount);
            }}
          >
            <label className="field-label">
              Recipient address (0x…)
              <input
                type="text"
                value={recipient}
                onChange={(event) => setRecipient(event.target.value)}
                placeholder="0x…"
                autoComplete="off"
                spellCheck={false}
              />
            </label>
            <label className="field-label">
              Amount (AVAX)
              <input
                type="text"
                value={amount}
                onChange={(event) => setAmount(event.target.value)}
                placeholder="1.5"
                autoComplete="off"
              />
            </label>
            {prepareError !== null ? <p className="form-error">{prepareError}</p> : null}
            <div className="form-actions">
              <button type="button" className="btn btn-ghost" onClick={onCancel}>
                Cancel
              </button>
              <button
                type="submit"
                className="btn btn-primary"
                disabled={recipient.trim().length === 0 || amount.trim().length === 0}
              >
                Build
              </button>
            </div>
          </form>
        </>
      ) : (
        <>
          <div className="panel-head">
            <h2 className="panel-heading">Confirm the transaction</h2>
            <span className="avax-network">{networkLabel(prepared.network)}</span>
          </div>
          <div className="avax-prepared">
            <PreparedLine label="From" value={prepared.from} />
            <PreparedLine label="To" value={prepared.to} />
            <PreparedLine label="Amount" value={`${avaxOf(prepared.valueWei)} AVAX`} />
            <PreparedLine
              label="Max fee"
              value={`${navaxOf(prepared.maxFeePerGas * BigInt(prepared.gasLimit))} nAVAX`}
            />
            <PreparedLine
              label="Max priority fee"
              value={`${navaxOf(prepared.maxPriorityFeePerGas)} nAVAX`}
            />
            <PreparedLine label="Gas limit" value={String(prepared.gasLimit)} />
            <PreparedLine label="Nonce" value={String(prepared.nonce)} />
            <PreparedLine label="Chain" value={networkLabel(prepared.network)} />
          </div>
          {prepared.network === 'mainnet' ? (
            <label className="avax-ack">
              <input
                type="checkbox"
                checked={acknowledged}
                onChange={(event) => onAcknowledged(event.target.checked)}
              />
              <span>
                This is mainnet AVAX — real money, sent to the address above, not reversible.
              </span>
            </label>
          ) : null}
          {sendError !== null ? <p className="form-error">{sendError}</p> : null}
          <div className="form-actions">
            <button type="button" className="btn btn-ghost" onClick={onCancel}>
              Back
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || (prepared.network === 'mainnet' && !acknowledged)}
              onClick={() => onSend(prepared)}
            >
              Confirm send
            </button>
          </div>
        </>
      )}
    </div>
  );
}
