// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract BatchSettlement {
    error LengthMismatch();
    error EmptyBatch();
    error ZeroRecipient();
    error InvalidValue();
    error AlreadySettled(bytes32 settlementId);
    error TransferFailed(address to, uint256 amount);

    event BatchSettled(bytes32 indexed settlementId, uint256 transferCount, uint256 totalAmount);

    mapping(bytes32 => bool) public settled;

    function settleBatch(
        address[] calldata recipients,
        uint256[] calldata amounts,
        bytes32 settlementId
    ) external payable {
        uint256 n = recipients.length;
        if (n == 0) revert EmptyBatch();
        if (n != amounts.length) revert LengthMismatch();
        if (settled[settlementId]) revert AlreadySettled(settlementId);

        uint256 total;
        unchecked {
            for (uint256 i = 0; i < n; i++) {
                address to = recipients[i];
                uint256 amount = amounts[i];
                if (to == address(0)) revert ZeroRecipient();
                total += amount;
            }
        }
        if (total != msg.value) revert InvalidValue();

        settled[settlementId] = true;
        for (uint256 i = 0; i < n; i++) {
            (bool ok,) = recipients[i].call{value: amounts[i]}("");
            if (!ok) revert TransferFailed(recipients[i], amounts[i]);
        }

        emit BatchSettled(settlementId, n, total);
    }
}

