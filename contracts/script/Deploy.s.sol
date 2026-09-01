// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "forge-std/Script.sol";
import "../src/VelaSettlement.sol";

contract Deploy is Script {
    function run() external {
        vm.startBroadcast();
        VelaSettlement settlement = new VelaSettlement(msg.sender);
        vm.stopBroadcast();

        console.log("VelaSettlement deployed to:", address(settlement));
        console.log("Operator:", msg.sender);
    }
}
