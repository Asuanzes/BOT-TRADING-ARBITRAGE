// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {PredictionMarket} from "../src/PredictionMarket.sol";

/// @notice Deploy `PredictionMarket` to Arbitrum Sepolia.
/// Usage:
///   forge script script/Deploy.s.sol \
///     --rpc-url arbitrum_sepolia \
///     --private-key $PRIVATE_KEY \
///     --broadcast --verify
///
/// `OPERATOR` env var sets the address allowed to call openRound/closeRound.
/// If unset, defaults to the deployer (msg.sender). Move it to the Chainlink
/// Automation upkeep address once that exists.
contract Deploy is Script {
    function run() external returns (PredictionMarket pm) {
        address operator = vm.envOr("OPERATOR", msg.sender);
        vm.startBroadcast();
        pm = new PredictionMarket(operator);
        vm.stopBroadcast();
        console.log("PredictionMarket deployed at:", address(pm));
        console.log("Operator:                    ", operator);
    }
}
