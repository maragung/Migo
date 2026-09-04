// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {AccessManager} from "@openzeppelin/contracts/access/manager/AccessManager.sol";
import {IAccessManager} from "@openzeppelin/contracts/access/manager/IAccessManager.sol";
import {MgoToken} from "../src/MgoToken.sol";
import {Treasury} from "../src/Treasury.sol";

/**
 * @title StorePayments deployment
 * @dev Deploys, in order: the AccessManager authority, the MGO token (implementation +
 * ERC-1967 proxy, initialized), and the Treasury. Reads `MIGO_DEPLOYER_PRIVATE_KEY`
 * (never logged), writes the addresses to `deployment.json` for the client constants and
 * the operator's audit notes to consume.
 *
 * The AccessManager's admin is `MIGO_ADMIN` (default: the deployer). The deployer is
 * granted MINTER and UPGRADER at zero delay — bootstrap roles meant to be re-scoped or
 * revoked through governed proposals once the operator takes over.
 */
contract StorePayments is Script {
    // The AccessManager's ids for the roles this deployment uses. GRANTED and revoked
    // through the authority, not hardcoded anywhere else: the role ids are part of the
    // deployment's public shape.
    uint64 public constant MINT_ROLE = 1;
    uint64 public constant UPGRADER_ROLE = 2;

    function run() public returns (MgoToken token, Treasury treasury) {
        uint256 deployerKey = vm.envUint("MIGO_DEPLOYER_PRIVATE_KEY");
        address admin = vm.envOr("MIGO_ADMIN", vm.addr(deployerKey));
        address deployer = vm.addr(deployerKey);

        vm.startBroadcast(deployerKey);

        AccessManager authority = new AccessManager(admin);
        authority.grantRole(MINT_ROLE, deployer, 0);
        authority.grantRole(UPGRADER_ROLE, deployer, 0);

        MgoToken implementation = new MgoToken();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(MgoToken.initialize, (IAccessManager(authority), admin))
        );
        token = MgoToken(address(proxy));

        treasury = new Treasury(admin);

        vm.stopBroadcast();

        // The deployment's record: what the store's constants and the operator's notes read.
        string memory json = string.concat(
            '{"network":"fuji","chainId":"',
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
