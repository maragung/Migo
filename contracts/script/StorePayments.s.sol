// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {AccessManager} from "@openzeppelin/contracts/access/manager/AccessManager.sol";
import {IAccessManager} from "@openzeppelin/contracts/access/manager/IAccessManager.sol";
import {MgoToken} from "../src/MgoToken.sol";
import {Treasury} from "../src/Treasury.sol";

/**
 * @title StorePayments deployment
 * @dev Deploys, in order: the AccessManager authority, the MGO token (implementation +
 * ERC-1967 proxy, initialized), and the Treasury — then shapes the authority and hands
 * the admin role to `MIGO_ADMIN`.
 *
 * Reads `MIGO_DEPLOYER_PRIVATE_KEY` (never logged), writes the addresses to
 * `deployment.json` (gitignored) — the exact list the store's placeholder constants wait
 * for.
 *
 * The deployer builds the authority as its *provisional* admin (an AccessManager
 * restricts every target function to ADMIN_ROLE until `setTargetFunctionRole` binds it):
 * the script binds `mint` to MINT_ROLE and the UUPS upgrade selectors to UPGRADER_ROLE
 * on the token proxy, grants those roles to the deployer at zero delay for the bootstrap
 * window, and finally grants ADMIN_ROLE to `MIGO_ADMIN` (default: the deployer) and
 * revokes its own — the operator wallet ends up holding the whole of the authority, the
 * deployer nothing but the two bootstrap roles it was meant to keep until the first
 * governed proposals re-scope them.
 */
contract StorePayments is Script {
  // The role ids this deployment defines. Part of the deployment's public shape:
  // the client's audit notes and any future proposal reference these numbers.
  uint64 public constant MINT_ROLE = 1;
  uint64 public constant UPGRADER_ROLE = 2;

  function run() public returns (MgoToken token, Treasury treasury) {
    uint256 deployerKey = vm.envUint("MIGO_DEPLOYER_PRIVATE_KEY");
    address admin = vm.envOr("MIGO_ADMIN", vm.addr(deployerKey));
    address deployer = vm.addr(deployerKey);

    vm.startBroadcast(deployerKey);

    // The deployer is the provisional admin; the handover is the script's last act.
    AccessManager authority = new AccessManager(deployer);

    MgoToken implementation = new MgoToken();
    ERC1967Proxy proxy =
      new ERC1967Proxy(address(implementation), abi.encodeCall(MgoToken.initialize, (IAccessManager(authority))));
    token = MgoToken(address(proxy));

    treasury = new Treasury(admin);

    // The authority's shape: bind the restricted functions to their roles (an unbound
    // function stays ADMIN_ROLE-only), label the roles for explorers, and grant the
    // bootstrap roles at zero delay — the deployment window's own access.
    bytes4[] memory mintSelectors = new bytes4[](1);
    mintSelectors[0] = MgoToken.mint.selector;
    bytes4[] memory upgradeSelectors = new bytes4[](2);
    upgradeSelectors[0] = UUPSUpgradeableProxy.upgradeTo.selector;
    upgradeSelectors[1] = UUPSUpgradeableProxy.upgradeToAndCall.selector;
    authority.labelRole(MINT_ROLE, "MINT_ROLE");
    authority.labelRole(UPGRADER_ROLE, "UPGRADER_ROLE");
    authority.setTargetFunctionRole(address(token), mintSelectors, MINT_ROLE);
    authority.setTargetFunctionRole(address(token), upgradeSelectors, UPGRADER_ROLE);
    authority.grantRole(MINT_ROLE, deployer, 0);
    authority.grantRole(UPGRADER_ROLE, deployer, 0);

    // The handover: the operator wallet takes ADMIN_ROLE, the deployer gives it up.
    // When they are the same wallet this is a no-op pair the chain never sees (the grant
    // would duplicate the role the deployer already holds as constructor admin).
    if (admin != deployer) {
      authority.grantRole(authority.ADMIN_ROLE(), admin, 0);
      authority.revokeRole(authority.ADMIN_ROLE(), deployer);
    }

    vm.stopBroadcast();

    // The deployment's record: what the store's constants and the operator's notes read.
    // The network label follows the chain the run actually landed on — a run against a
    // local anvil says anvil, so a mistaken RPC is visible in the file itself.
    string memory network = block.chainid == 31_337 ? "anvil" : block.chainid == 43_113 ? "fuji" : "unknown";
    string memory json = string.concat(
      '{"network":"',
      network,
      '","chainId":"',
      vm.toString(block.chainid),
      '","deployer":"',
      vm.toString(deployer),
      '","admin":"',
      vm.toString(admin),
      '","accessManager":"',
      vm.toString(address(authority)),
      '","mgoToken":"',
      vm.toString(address(token)),
      '","treasury":"',
      vm.toString(address(treasury)),
      '"}'
    );
    vm.writeJson(json, "deployment.json");
    console2.log("AccessManager:", address(authority));
    console2.log("MgoToken    :", address(token));
    console2.log("Treasury    :", address(treasury));
    console2.log("deployment written to contracts/deployment.json");
  }
}

/// @dev The UUPS selectors the upgrade binding names, spelled once so the script reads
/// as what it binds. The proxy exposes the UUPSUpgradeable interface.
interface UUPSUpgradeableProxy {
  function upgradeTo(address newImplementation) external;
  function upgradeToAndCall(address newImplementation, bytes memory data) external payable;
}
