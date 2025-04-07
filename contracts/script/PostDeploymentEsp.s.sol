// SPDX-License-Identifier: Unlicensed
pragma solidity ^0.8.0;

import "forge-std/Script.sol";
import "../src/EspToken.sol";

contract PostDeploymentEsp is Script {
    error InvalidName(string expected, string actual);
    error InvalidSymbol(string expected, string actual);
    error InvalidOwner(address expected, address actual);
    error InvalidTotalSupply(uint256 expected, uint256 actual);
    error InvalidInitialGrantRecipientBalance(uint256 expected, uint256 actual);
    error InvalidVersion();

    function run(
        address proxyAddress,
        address owner,
        uint256 totalSupply,
        address initialGrantRecipient,
        string memory name,
        string memory symbol
    ) external {
        EspToken token = EspToken(proxyAddress);

        if (keccak256(bytes(token.name())) != keccak256(bytes(name))) {
            revert InvalidName(name, token.name());
        }
        if (keccak256(bytes(token.symbol())) != keccak256(bytes(symbol))) {
            revert InvalidSymbol(symbol, token.symbol());
        }
        if (token.owner() != owner) {
            revert InvalidOwner(owner, token.owner());
        }
        if (token.totalSupply() != totalSupply) {
            revert InvalidTotalSupply(totalSupply, token.totalSupply());
        }
        if (token.balanceOf(initialGrantRecipient) != totalSupply) {
            revert InvalidInitialGrantRecipientBalance(
                totalSupply, token.balanceOf(initialGrantRecipient)
            );
        }
        (uint8 major, uint8 minor, uint8 patch) = token.getVersion();
        if (major != 1 || minor != 0 || patch != 0) {
            revert InvalidVersion();
        }
    }
}
