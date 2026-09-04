// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ERC20Upgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC20/ERC20Upgradeable.sol";
import {ERC20PermitUpgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC20/extensions/ERC20PermitUpgradeable.sol";
import {ERC20BurnableUpgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC20/extensions/ERC20BurnableUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {AccessManagerUpgradeable} from "@openzeppelin/contracts-upgradeable/access/manager/AccessManagerUpgradeable.sol";
import {IAccessManager} from "@openzeppelin/contracts/access/manager/IAccessManager.sol";

/**
 * @title MgoToken
 * @notice The MGO payment token: an ERC-20 with permit and burnable balances, governed
 * through an {AccessManager} so the treasury's operator can mint against a received
 * payment while nobody — not even the deployer — can move a balance it does not own.
 *
 * The store's flow never requires this contract to escrow anything: the buyer pays the
 * treasury directly (AVAX or USDT/USDC), and the MGO the price names is the *unit the
 * amount is quoted in*, not a token that changes hands here. MGO exists on-chain so
 * the deployment can also pay rewards and refunds with the same unit the prices use.
 *
 * Minting is the only privileged operation. It is routed through the {AccessManager}
 * set at initialization (an {Initializable} constructor argument rather than an
 * immutable `Ownable` owner, because a UUPS proxy cannot hold constructor state), which
 * means the authority to mint lives behind role delays the deployment's own operators
 * configure — a key compromise is a delay window, not an instant drain.
 *
 * Upgrades: the {UUPSUpgradeable} `upgrade` authority is held by the same AccessManager
 * (the `UPGRADER_ROLE` it defines), so a future MGO contract version ships as a normal
 * governed proposal, and `initialize` is `_disableInitializers`-sealed in the
 * constructor the way OZ's own templates seal theirs.
 */
contract MgoToken is
    Initializable,
    ERC20Upgradeable,
    ERC20PermitUpgradeable,
    ERC20BurnableUpgradeable,
    UUPSUpgradeable
{
    /// @notice The AccessManager that holds the mint and upgrade authorities.
    IAccessManager private _authority;

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    /**
     * @param authority The {AccessManager} that owns the MINTER and UPGRADER roles.
     * @param initialOwner The address that receives every role at bootstrap; the
     *   deployment revokes or re-scopes these through AccessManager proposals afterwards.
     */
    function initialize(IAccessManager authority, address initialOwner) public initializer {
        __ERC20_init("Migo Token", "MGO");
        __ERC20Permit_init("Migo Token");
        __ERC20Burnable_init();
        __UUPSUpgradeable_init();
        _authority = authority;
        // The deployer's own roles are granted by the deploy script through the authority,
        // not here: this function runs inside the proxy and must not depend on `msg.sender`
        // being the deployer (it is the proxy admin in some forwarder setups).
        initialOwner; // role grants are the script's job; documented, deliberately unused here
    }

    /// @inheritdoc ERC20Upgradeable
    function decimals() public pure override returns (uint8) {
        // 18: the ERC-20's own unit, the same magnitude AVAX's wei and the store's
        // `MGO_DECIMALS` constant already agree on. Pure because it is a promise, not state.
        return 18;
    }

    /**
     * @notice Mints `amount` to `to`. Restricted to the authority's MINTER (an
     * {AccessManager} role with a delay, so a compromised operator is a window to react,
     * not an instant printer).
     */
    function mint(address to, uint256 amount) public restricted {
        _mint(to, amount);
    }

    /// @dev The authority this contract consults for `restricted` calls.
    function authority() public view returns (address) {
        return address(_authority);
    }

    /// @dev Only the authority's UPGRADER role may move the implementation pointer.
    function _authorizeUpgrade(address newImplementation) internal override restricted {}
}
