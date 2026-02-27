// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import "../src/BatchSettlement.sol";

contract DeployBatchSettlement is Script {
    function run() external returns (BatchSettlement deployed) {
        uint256 deployerPk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        vm.startBroadcast(deployerPk);
        deployed = new BatchSettlement();
        vm.stopBroadcast();
    }
}

