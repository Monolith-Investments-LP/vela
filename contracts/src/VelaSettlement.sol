// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";

contract VelaSettlement is ReentrancyGuard {
    using SafeERC20 for IERC20;
    using ECDSA for bytes32;
    using MessageHashUtils for bytes32;

    address public operator;
    uint256 public constant EMERGENCY_DELAY = 7 days;

    struct Balance {
        uint256 amount;
        uint256 emergencyUnlockAt;
    }

    // user => asset => Balance
    mapping(address => mapping(address => Balance)) public balances;

    // user => nonce => used
    // Prevents replay of a valid operator withdrawal signature.
    mapping(address => mapping(uint256 => bool)) public usedWithdrawNonces;

    // ETH represented as address(0)
    address public constant ETH = address(0);

    mapping(uint256 => bytes32) public anchoredStateRoots;
    uint256 public anchorCount;

    error ZeroAmount();
    error UseDepositETHForNative();
    error InsufficientBalance();
    error InvalidSignature();
    error NonceAlreadyUsed();
    error NoBalance();
    error EmergencyExitNotInitiated();
    error TimelockActive();
    error EthTransferFailed();
    error NotOperator();

    event Deposited(address indexed user, address indexed asset, uint256 amount);
    event Withdrawn(address indexed user, address indexed asset, uint256 amount, uint256 nonce);
    event EmergencyExitInitiated(address indexed user, address indexed asset, uint256 unlockAt);
    event EmergencyExitExecuted(address indexed user, address indexed asset, uint256 amount);
    event StateRootAnchored(uint256 indexed anchorId, bytes32 stateRoot, uint256 timestamp, uint256 ordersProcessed);

    constructor(address _operator) {
        operator = _operator;
    }

    modifier onlyOperator() {
        if (msg.sender != operator) revert NotOperator();
        _;
    }

    function depositETH() external payable nonReentrant {
        if (msg.value == 0) revert ZeroAmount();
        balances[msg.sender][ETH].amount += msg.value;
        emit Deposited(msg.sender, ETH, msg.value);
    }

    function depositToken(address asset, uint256 amount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        if (asset == ETH) revert UseDepositETHForNative();
        IERC20(asset).safeTransferFrom(msg.sender, address(this), amount);
        balances[msg.sender][asset].amount += amount;
        emit Deposited(msg.sender, asset, amount);
    }

    /// @notice Hash the operator commits to for `withdraw`.
    /// Includes `address(this)` and `block.chainid` for domain separation,
    /// so signatures cannot be replayed against a different Vela deployment
    /// or a different chain.
    function withdrawHash(address user, address asset, uint256 amount, uint256 nonce) public view returns (bytes32) {
        bytes32 inner = keccak256(abi.encodePacked(user, asset, amount, nonce, block.chainid, address(this)));
        return inner.toEthSignedMessageHash();
    }

    function withdraw(address asset, uint256 amount, uint256 nonce, bytes calldata signature) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        if (balances[msg.sender][asset].amount < amount) revert InsufficientBalance();
        if (usedWithdrawNonces[msg.sender][nonce]) revert NonceAlreadyUsed();

        bytes32 hash = withdrawHash(msg.sender, asset, amount, nonce);
        // ECDSA.recover reverts on malleable (high-s) or malformed signatures.
        address signer = hash.recover(signature);
        if (signer != operator) revert InvalidSignature();

        // Effects: consume nonce and debit balance before external interaction.
        usedWithdrawNonces[msg.sender][nonce] = true;
        balances[msg.sender][asset].amount -= amount;

        if (asset == ETH) {
            (bool ok,) = msg.sender.call{ value: amount }("");
            if (!ok) revert EthTransferFailed();
        } else {
            IERC20(asset).safeTransfer(msg.sender, amount);
        }

        emit Withdrawn(msg.sender, asset, amount, nonce);
    }

    function initiateEmergencyExit(address asset) external {
        if (balances[msg.sender][asset].amount == 0) revert NoBalance();
        uint256 unlockAt = block.timestamp + EMERGENCY_DELAY;
        balances[msg.sender][asset].emergencyUnlockAt = unlockAt;
        emit EmergencyExitInitiated(msg.sender, asset, unlockAt);
    }

    function executeEmergencyExit(address asset) external nonReentrant {
        Balance storage bal = balances[msg.sender][asset];
        if (bal.amount == 0) revert NoBalance();
        if (bal.emergencyUnlockAt == 0) revert EmergencyExitNotInitiated();
        if (block.timestamp < bal.emergencyUnlockAt) revert TimelockActive();

        uint256 amount = bal.amount;
        bal.amount = 0;
        bal.emergencyUnlockAt = 0;

        if (asset == ETH) {
            (bool ok,) = msg.sender.call{ value: amount }("");
            if (!ok) revert EthTransferFailed();
        } else {
            IERC20(asset).safeTransfer(msg.sender, amount);
        }

        emit EmergencyExitExecuted(msg.sender, asset, amount);
    }

    function anchorStateRoot(bytes32 stateRoot, uint256 ordersProcessed) external onlyOperator {
        uint256 anchorId = anchorCount++;
        anchoredStateRoots[anchorId] = stateRoot;
        emit StateRootAnchored(anchorId, stateRoot, block.timestamp, ordersProcessed);
    }

    function latestAnchoredStateRoot()
        external
        view
        returns (uint256 anchorId, bytes32 stateRoot, uint256 anchorCount_)
    {
        if (anchorCount == 0) {
            return (0, bytes32(0), 0);
        }
        anchorId = anchorCount - 1;
        stateRoot = anchoredStateRoots[anchorId];
        anchorCount_ = anchorCount;
    }

    function getBalance(address user, address asset) external view returns (uint256) {
        return balances[user][asset].amount;
    }
}
