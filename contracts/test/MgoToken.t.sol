// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {AccessManager} from "@openzeppelin/contracts/access/manager/AccessManager.sol";
import {IAccessManager} from "@openzeppelin/contracts/access/manager/IAccessManager.sol";
import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
import {ERC20Mock} from "./mocks/ERC20Mock.sol";
import {MgoToken} from "../src/MgoToken.sol";
import {Treasury} from "../src/Treasury.sol";

/**
 * The MGO token and treasury under test: the shapes the store actually drives, plus the
 * shapes it must refuse.
 *
 * `authority`, `token`, `payer`, and `treasury` are the fixtures every test reuses; the
 * payer holds MGO balances (via the authority's mint) and AVAX (via `deal`), the way the
 * store's wallet-0 holder does.
 */
contract MgoTokenTest is Test {
  MgoToken internal token;
  Treasury internal treasury;
  AccessManager internal authority;

  address internal deployer = makeAddr("deployer");
  address internal admin = makeAddr("admin");
  address internal payer = makeAddr("payer");
  address internal attacker = makeAddr("attacker");

  uint64 internal constant MINT_ROLE = 1;
  uint64 internal constant UPGRADER_ROLE = 2;

  function setUp() public virtual {
    vm.prank(deployer);
    authority = new AccessManager(admin);

    vm.prank(deployer);
    MgoToken implementation = new MgoToken();
    vm.prank(deployer);
    ERC1967Proxy proxy =
      new ERC1967Proxy(address(implementation), abi.encodeCall(MgoToken.initialize, (IAccessManager(authority))));
    token = MgoToken(address(proxy));

    // The authority's shape, the way the deploy script builds it: bind mint to
    // MINT_ROLE and the UUPS selectors to UPGRADER_ROLE (an unbound function stays
    // ADMIN_ROLE-only), then grant the bootstrap roles to the deployer.
    bytes4[] memory mintSelectors = new bytes4[](1);
    mintSelectors[0] = MgoToken.mint.selector;
    bytes4[] memory upgradeSelectors = new bytes4[](2);
    upgradeSelectors[0] = Proxy.upgradeTo.selector;
    upgradeSelectors[1] = Proxy.upgradeToAndCall.selector;
    vm.startPrank(admin);
    authority.setTargetFunctionRole(address(token), mintSelectors, MINT_ROLE);
    authority.setTargetFunctionRole(address(token), upgradeSelectors, UPGRADER_ROLE);
    authority.grantRole(MINT_ROLE, deployer, 0);
    authority.grantRole(UPGRADER_ROLE, deployer, 0);
    vm.stopPrank();

    vm.prank(deployer);
    treasury = new Treasury(admin);
  }

  /// The deployment's shape: 18 decimals, the name and symbol the store quotes.
  function test_erc20_shape() public view {
    assertEq(token.name(), "Migo Token");
    assertEq(token.symbol(), "MGO");
    assertEq(token.decimals(), 18);
  }

  /// The authority's minter mints; nobody else does — the contract's one privileged
  /// operation, closed to the outside.
  function test_mint_requires_role() public {
    vm.prank(deployer);
    token.mint(payer, 150 ether);
    assertEq(token.balanceOf(payer), 150 ether);

    vm.prank(attacker);
    vm.expectRevert();
    token.mint(attacker, 1 ether);
  }

  /// A holder burns their own balance; the store's buyers never hold MGO they did not
  /// buy, but the operation the ERC-20 promises is the operation the contract keeps.
  function test_burn_own_balance_only() public {
    vm.startPrank(deployer);
    token.mint(payer, 100 ether);
    vm.stopPrank();

    vm.prank(attacker);
    vm.expectRevert(abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, attacker, 0, 100 ether));
    token.burn(100 ether);

    vm.prank(payer);
    token.burn(40 ether);
    assertEq(token.balanceOf(payer), 60 ether);
  }

  /// Transfers are plain ERC-20: no tax, no pause, no hook — the store's price and the
  /// chain's amount must never disagree by a fee the token invents.
  function test_transfer_moves_face_value() public {
    vm.prank(deployer);
    token.mint(payer, 150 ether);

    vm.prank(payer);
    token.transfer(address(treasury), 150 ether);
    assertEq(token.balanceOf(address(treasury)), 150 ether);
    assertEq(token.balanceOf(payer), 0);
  }

  /// Permit: the store's off-chain approvals (if a future flow needs one) sign an
  /// allowance instead of paying gas for it. Exercise the EIP-2612 path end to end.
  /// The signer *is* the owner — a permit is the owner's own signature, so the test
  /// derives its payer from the key it signs with rather than the fixture address.
  function test_permit_grants_allowance() public {
    uint256 holderKey = 0xa11ce;
    address holder = vm.addr(holderKey);
    vm.prank(deployer);
    token.mint(holder, 10 ether);

    // EIP-2612's fixed struct hash (the permit's shape is the standard's, not the
    // implementation's): keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)")
    bytes32 permitTypehash = 0x6e71edae12b1b97f4d1f60370fef10105fa2faae0126114a169c64845d6126c9;
    (uint8 v, bytes32 r, bytes32 s) = vm.sign(
      holderKey,
      keccak256(
        abi.encodePacked(
          "\x19\x01",
          token.DOMAIN_SEPARATOR(),
          keccak256(abi.encode(permitTypehash, holder, address(treasury), 10 ether, 0, block.timestamp))
        )
      )
    );
    token.permit(holder, address(treasury), 10 ether, block.timestamp, v, r, s);
    assertEq(token.allowance(holder, address(treasury)), 10 ether);
  }

  /// The constructor sealed `initialize`: a second call reverts, the way an upgradeable
  /// contract's un-initialized proxy hijack requires.
  function test_reinitialize_refused() public {
    vm.expectRevert();
    token.initialize(IAccessManager(authority));
  }
}

contract TreasuryTest is Test {
  Treasury internal treasury;
  ERC20Mock internal usdt;

  address internal admin = makeAddr("admin");
  address internal payer = makeAddr("payer");
  address internal attacker = makeAddr("attacker");

  function setUp() public virtual {
    vm.prank(admin);
    treasury = new Treasury(admin);
    usdt = new ERC20Mock();
    usdt.mint(payer, 1_000 ether);
    vm.deal(payer, 1_000 ether);
  }

  /// The store's AVAX shape: a plain value transfer. It lands, it is logged with the
  /// payer the transaction names, and the AVAX stays put until the owner sweeps.
  function test_native_payment_lands_and_logs() public {
    vm.expectEmit(true, true, true, true);
    emit PaymentReceived(address(0), payer, 150 ether, "");
    vm.prank(payer);
    (bool ok,) = address(treasury).call{value: 150 ether}("");
    assertTrue(ok);
    assertEq(address(treasury).balance, 150 ether);
  }

  /// The store's token shape: the payer approves and `pay`s with the reference the
  /// store's idempotency key rides in — the chain's log and the server's ledger
  /// reconcile line by line.
  function test_token_payment_records_reference() public {
    vm.prank(payer);
    usdt.approve(address(treasury), 150 ether);
    vm.expectEmit(true, true, true, true);
    emit PaymentReceived(address(usdt), payer, 150 ether, "frog_set:0xabc");
    vm.prank(payer);
    treasury.pay(usdt, 150 ether, "frog_set:0xabc");
    assertEq(usdt.balanceOf(address(treasury)), 150 ether);
  }

  /// A zero-amount `pay` is refused: the log's readers must never have to wonder
  /// whether an entry with amount zero was a payment or a probe.
  function test_zero_payment_refused() public {
    vm.prank(payer);
    vm.expectRevert("Treasury: zero amount");
    treasury.pay(usdt, 0, "");
  }

  /// `pay` without approval reverts inside SafeERC20 — the pull never half-happens.
  function test_pay_without_allowance_reverts() public {
    vm.prank(payer);
    vm.expectRevert();
    treasury.pay(usdt, 1 ether, "");
  }

  /// Sweeps: the whole balance, only the owner, always logged. Everyone else is refused.
  function test_sweep_native_only_owner() public {
    vm.prank(payer);
    (bool ok,) = address(treasury).call{value: 150 ether}("");
    assertTrue(ok);

    vm.prank(attacker);
    vm.expectRevert();
    treasury.sweepNative(attacker);

    vm.prank(admin);
    vm.expectEmit(true, true, true, true);
    emit Swept(address(0), admin, 150 ether);
    treasury.sweepNative(admin);
    assertEq(address(treasury).balance, 0);
    assertEq(admin.balance, 150 ether);
  }

  function test_sweep_token_only_owner() public {
    vm.startPrank(payer);
    usdt.approve(address(treasury), 200 ether);
    treasury.pay(usdt, 200 ether, "");
    vm.stopPrank();

    vm.prank(attacker);
    vm.expectRevert();
    treasury.sweepToken(usdt, attacker);

    vm.prank(admin);
    vm.expectEmit(true, true, true, true);
    emit Swept(address(usdt), admin, 200 ether);
    treasury.sweepToken(usdt, admin);
    assertEq(usdt.balanceOf(admin), 200 ether);
    assertEq(usdt.balanceOf(address(treasury)), 0);
  }
}

// The events the expectEmit checks name, declared at file scope so both suites see them.
event PaymentReceived(address indexed token, address indexed payer, uint256 amount, string note);
event Swept(address indexed token, address indexed to, uint256 amount);

/// @dev The UUPS selectors the role binding names — the proxy's upgrade surface.
interface Proxy {
  function upgradeTo(address newImplementation) external;
  function upgradeToAndCall(address newImplementation, bytes memory data) external payable;
}
