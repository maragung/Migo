// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Address} from "@openzeppelin/contracts/utils/Address.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

/**
 * @title Treasury
 * @notice The address the store's on-chain payments land in.
 *
 * The buyer pays one of two shapes, both of which arrive at this contract:
 *
 *   * AVAX — a plain native-value transfer to the contract address (the store's confirm
 *     screen quotes it as the `to` line), which lands in `receive()`.
 *   * USDT/USDC — an ERC-20 `transfer` whose recipient is this contract, which the token
 *     contract executes against this contract's balance with no callback here.
 *
 * Every arrival is logged: `PaymentReceived` carries the token (address zero = AVAX),
 * the payer the client quotes, the amount, and a `reference` the store sets to the
 * purchase's idempotency key — `"<sku>:<txHash>"` — so the server's entitlement ledger and
 * the chain's own log reconcile line by line without the treasury needing to know what a
 * SKU is.
 *
 * The contract deliberately does *no* accounting logic, *no* conversion, and holds *no*
 * price table: the price is what the buyer's own transaction said, and the entitlement is
 * the Migo server's to write after the chain confirms. This keeps the contract's promise
 * small — receive, record, sweep — which is the whole of what a payment address should
 * promise on-chain.
 *
 * Withdrawals are owner-sweeps, not per-payment taps: the owner moves whole balances of a
 * token (or the whole AVAX balance) out, and every sweep is logged with the same
 * `PaymentReceived`-style completeness. A {ReentrancyGuard} on the sweep path plus
 * {SafeERC20}/{Address} calls-only send keep the usual drain shapes out.
 */
contract Treasury is Ownable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    /// @notice Emitted for every value arrival, native or token.
    event PaymentReceived(
        /// The token the payment was made in: address zero for native AVAX.
        address indexed token,
        /// The payer, as the payment's own transaction names them.
        address indexed payer,
        /// The amount, in the token's smallest unit.
        uint256 amount,
        /// The store's idempotency key (`sku:txHash`) when the payer sent one; the
        /// transfer's calldata or the payer's own reference otherwise. A direct AVAX send
        /// with no data carries the empty string.
        string reference
    );

    /// @notice Emitted for every owner sweep.
    event Swept(
        /// The token swept: address zero for native AVAX.
        address indexed token,
        /// Where the balance went.
        address indexed to,
        /// The whole balance moved.
        uint256 amount
    );

    /// @param initialOwner The address that may sweep balances. The deploy script sets
    ///   this to the deployment's operator wallet; ownership transfers follow the
    ///   standard two-step Ownable flow.
    constructor(address initialOwner) Ownable(initialOwner) {}

    /// @notice AVAX arrivals. Plain value transfers land here; the reference is whatever
    ///   calldata the sender attached (the store attaches none — its key is the tx hash).
    receive() external payable {
        emit PaymentReceived(address(0), msg.sender, msg.value, "");
    }

    /// @notice Token arrivals have no hook: a payment is *recognised* when the payer (or
    /// the store, off-chain) logs it. The treasury keeps this function so a payer who
    /// wants the on-chain log to carry a reference can make it explicit: the payer
    /// approves, then calls `pay(token, amount, reference)` and this contract pulls.
    function pay(IERC20 token, uint256 amount, string calldata reference) external nonReentrant {
        require(amount > 0, "Treasury: zero amount");
        SafeERC20.safeTransferFrom(token, msg.sender, address(this), amount);
        emit PaymentReceived(address(token), msg.sender, amount, reference);
    }

    /// @notice Moves the whole AVAX balance out. Only the owner, never during a payment.
    function sweepNative(address to) external onlyOwner nonReentrant {
        uint256 amount = address(this).balance;
        require(amount > 0, "Treasury: nothing to sweep");
        Address.sendValue(payable(to), amount);
        emit Swept(address(0), to, amount);
    }

    /// @notice Moves the whole balance of `token` out. Only the owner, never during a
    ///   pull-payment (the {SafeERC20} calls guard the arbitrary-token shapes).
    function sweepToken(IERC20 token, address to) external onlyOwner nonReentrant {
        uint256 amount = token.balanceOf(address(this));
        require(amount > 0, "Treasury: nothing to sweep");
        SafeERC20.safeTransfer(token, to, amount);
        emit Swept(address(token), to, amount);
    }
}
