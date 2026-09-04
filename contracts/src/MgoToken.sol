// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ERC20Upgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC20/ERC20Upgradeable.sol";
import {
  ERC20PermitUpgradeable
} from "@openzeppelin/contracts-upgradeable/token/ERC20/extensions/ERC20PermitUpgradeable.sol";
import {
  ERC20BurnableUpgradeable
} from "@openzeppelin/contracts-upgradeable/token/ERC20/extensions/ERC20BurnableUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {
  AccessManagedUpgradeable
} from "@openzeppelin/contracts-upgradeable/access/manager/AccessManagedUpgradeable.sol";
import {IAccessManager} from "@openzeppelin/contracts/access/manager/IAccessManager.sol";

/**
 * @title MgoToken
 * @notice The MGO payment token: an ERC-20 with permit and burnable balances, governed
 * through an {IAccessManager} so the deployment's operator can mint while nobody — not
 * even the deployer — can move a balance they do not own.
 *
 * The store's flow never requires this contract to escrow anything: the buyer pays the
 * treasury directly (AVAX or USDT/USDC), and the MGO the price names is the *unit the
 * amount is quoted in*, not a token that changes hands here. MGO exists on-chain so the
 * deployment can also pay rewards and refunds with the same unit the prices use.
 *
 * Minting and upgrading are the only privileged operations. They are routed through the
 * {IAccessManager} set at initialization — an {Initializable} argument rather than an
 * `Ownable` owner, because a UUPS proxy cannot hold constructor state — so the authority
 * to mint lives behind role delays the deployment's own operators configure: a key
 * compromise is a delay window to react in, not an instant printer.
 *
 * The {UUPSUpgradeable} upgrade authority is the same AccessManager (its UPGRADER role),
 * so a future MGO contract version ships as a normal governed proposal, and
 * `initialize` is `_disableInitializers`-sealed in the constructor the way OZ's own
 * templates seal theirs.
 */
contract MgoToken is
  Initializable,
  ERC20Upgradeable,
  ERC20PermitUpgradeable,
  ERC20BurnableUpgradeable,
  AccessManagedUpgradeable,
  UUPSUpgradeable
{
  /// @custom:oz-upgrades-unsafe-allow constructor
  constructor() {
    _disableInitializers();
  }

  /**
   * @param authority The {IAccessManager} that holds the MINT and UPGRADER roles.
   */
  function initialize(IAccessManager authority) public initializer {
    __ERC20_init("Migo Token", "MGO");
    __ERC20Permit_init("Migo Token");
    __ERC20Burnable_init();
    __AccessManaged_init(address(authority));
    // UUPSUpgradeable v5 needs no initializer: its "state" is the ERC-1967 slot the
    // proxy itself owns, and nothing here to initialize.
  }

  /// @inheritdoc ERC20Upgradeable
  function decimals() public pure override returns (uint8) {
    // 18: the ERC-20's own unit, the same magnitude AVAX's wei and the store's
    // `MGO_DECIMALS` constant already agree on. Pure because it is a promise, not state.
    return 18;
  }

  /**
   * @notice Mints `amount` to `to`. `onlyRole`-gated by the authority's MINT (an
   * AccessManager role with a delay), via {AccessManagedUpgradeable}'s `restricted`
   * modifier — the role id is this deployment's public shape, declared here.
   */
  uint64 public constant MINT_ROLE = 1;

  function mint(address to, uint256 amount) external restricted {
    _mint(to, amount);
  }

  /// @dev Only the authority's UPGRADER may move the implementation pointer.
  uint64 public constant UPGRADER_ROLE = 2;

  function _authorizeUpgrade(address newImplementation) internal override restricted {}
}
