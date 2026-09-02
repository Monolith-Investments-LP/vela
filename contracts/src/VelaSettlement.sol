// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import "@openzeppelin/contracts/access/Ownable2Step.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";

/// @notice Minimal Chainlink L2 sequencer-uptime feed interface.
/// Only used when `sequencerUptimeFeed` is set to a non-zero address —
/// L1 deployments leave the address at zero and skip the check.
interface IL2SequencerUptimeFeed {
    /// Returns the latest round: (roundId, answer, startedAt, updatedAt, answeredInRound).
    /// `answer == 0` means the sequencer is UP; `answer == 1` means DOWN.
    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
}

/// @title VelaSettlement
/// @notice On-chain custody for the Vela exchange. Users deposit ETH or
/// ERC20 tokens; the operator co-signs withdrawals; if the operator
/// goes silent, users can escape via a 7-day emergency exit.
///
/// Hardening notes (item 6 of the 2026-09-02 gap audit):
/// - `Ownable2Step` for owner rotation with an explicit accept step.
/// - `Pausable`. `pause()` is callable by either owner or guardian for
///   quick response; `unpause()` is owner-only, so a compromised
///   guardian cannot un-pause after halting withdrawals.
/// - Operator rotation goes through a 48h timelock via
///   `proposeOperator` → `acceptProposedOperator`, so a stolen owner
///   key cannot atomically hand the operator role to an attacker.
/// - Optional Chainlink L2 sequencer-uptime check on the fund-moving
///   paths (`withdraw`, `executeEmergencyExit`, `anchorStateRoot`).
///   Zero address disables the check — L1 mainnet deployments skip it.
contract VelaSettlement is ReentrancyGuard, Ownable2Step, Pausable {
    using SafeERC20 for IERC20;
    using ECDSA for bytes32;
    using MessageHashUtils for bytes32;

    address public operator;
    /// Address permitted to `pause()` without owner intervention.
    /// Distinct from `operator` so a stolen matching-engine key cannot
    /// halt withdrawals, and distinct from `owner()` so pausing does
    /// not require a slow governance step.
    address public guardian;

    /// Optional Chainlink L2 sequencer-uptime feed. Zero means "check
    /// disabled" (L1). Set via `setSequencerFeed(...)` after deployment
    /// on Arbitrum / Base / Optimism.
    IL2SequencerUptimeFeed public sequencerUptimeFeed;
    /// Additional cooldown after the sequencer comes back up before
    /// fund-moving paths accept requests again.
    uint256 public constant SEQUENCER_GRACE_PERIOD = 1 hours;

    /// Operator rotation queue. `proposeOperator` sets both fields;
    /// `acceptProposedOperator` clears them after the timelock.
    address public pendingOperator;
    uint256 public pendingOperatorEta;
    uint256 public constant OPERATOR_ROTATION_DELAY = 48 hours;

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
    error NotGuardianOrOwner();
    error SequencerDown();
    error SequencerGraceActive();
    error NoPendingOperator();
    error OperatorTimelockActive();
    error ZeroAddress();

    event Deposited(address indexed user, address indexed asset, uint256 amount);
    event Withdrawn(address indexed user, address indexed asset, uint256 amount, uint256 nonce);
    event EmergencyExitInitiated(address indexed user, address indexed asset, uint256 unlockAt);
    event EmergencyExitExecuted(address indexed user, address indexed asset, uint256 amount);
    event StateRootAnchored(uint256 indexed anchorId, bytes32 stateRoot, uint256 timestamp, uint256 ordersProcessed);
    event OperatorProposed(address indexed proposed, uint256 eta);
    event OperatorRotated(address indexed previousOperator, address indexed newOperator);
    event GuardianRotated(address indexed previousGuardian, address indexed newGuardian);
    event SequencerFeedUpdated(address indexed feed);

    constructor(address _operator) Ownable(msg.sender) {
        if (_operator == address(0)) revert ZeroAddress();
        operator = _operator;
        // Default the guardian to the deployer. Owner can rotate later
        // (e.g. to a multisig / on-call rota).
        guardian = msg.sender;
    }

    modifier onlyOperator() {
        if (msg.sender != operator) revert NotOperator();
        _;
    }

    modifier onlyGuardianOrOwner() {
        if (msg.sender != guardian && msg.sender != owner()) {
            revert NotGuardianOrOwner();
        }
        _;
    }

    /// @dev Fail closed on any fund-moving path if the L2 sequencer is
    /// reported down (or came back up within the grace window). No-op
    /// when `sequencerUptimeFeed` is unset.
    modifier sequencerUp() {
        _checkSequencer();
        _;
    }

    // -----------------------------------------------------------------
    // Deposits
    // -----------------------------------------------------------------

    function depositETH() external payable nonReentrant whenNotPaused {
        if (msg.value == 0) revert ZeroAmount();
        balances[msg.sender][ETH].amount += msg.value;
        emit Deposited(msg.sender, ETH, msg.value);
    }

    function depositToken(address asset, uint256 amount) external nonReentrant whenNotPaused {
        if (amount == 0) revert ZeroAmount();
        if (asset == ETH) revert UseDepositETHForNative();
        IERC20(asset).safeTransferFrom(msg.sender, address(this), amount);
        balances[msg.sender][asset].amount += amount;
        emit Deposited(msg.sender, asset, amount);
    }

    // -----------------------------------------------------------------
    // Operator-signed withdrawal
    // -----------------------------------------------------------------

    /// @notice Hash the operator commits to for `withdraw`.
    /// Includes `address(this)` and `block.chainid` for domain separation,
    /// so signatures cannot be replayed against a different Vela deployment
    /// or a different chain.
    function withdrawHash(address user, address asset, uint256 amount, uint256 nonce)
        public
        view
        returns (bytes32)
    {
        bytes32 inner = keccak256(abi.encodePacked(user, asset, amount, nonce, block.chainid, address(this)));
        return inner.toEthSignedMessageHash();
    }

    function withdraw(address asset, uint256 amount, uint256 nonce, bytes calldata signature)
        external
        nonReentrant
        whenNotPaused
        sequencerUp
    {
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

    // -----------------------------------------------------------------
    // Emergency exit — deliberately callable even while paused so a
    // pause-and-abandon scenario can never trap user funds. Sequencer
    // check is still enforced on the execute leg for L2.
    // -----------------------------------------------------------------

    function initiateEmergencyExit(address asset) external {
        if (balances[msg.sender][asset].amount == 0) revert NoBalance();
        uint256 unlockAt = block.timestamp + EMERGENCY_DELAY;
        balances[msg.sender][asset].emergencyUnlockAt = unlockAt;
        emit EmergencyExitInitiated(msg.sender, asset, unlockAt);
    }

    function executeEmergencyExit(address asset) external nonReentrant sequencerUp {
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

    // -----------------------------------------------------------------
    // Operator: state-root anchor
    // -----------------------------------------------------------------

    function anchorStateRoot(bytes32 stateRoot, uint256 ordersProcessed)
        external
        onlyOperator
        whenNotPaused
        sequencerUp
    {
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

    // -----------------------------------------------------------------
    // Admin: pause / unpause
    // -----------------------------------------------------------------

    /// @notice Pause fund-moving user paths. Guardian or owner can call
    /// this at any time; a paused contract still permits
    /// `initiateEmergencyExit`, `executeEmergencyExit`, and view calls.
    function pause() external onlyGuardianOrOwner {
        _pause();
    }

    /// @notice Only the owner can un-pause. Split from `pause()` so a
    /// compromised guardian can't bring the contract back online.
    function unpause() external onlyOwner {
        _unpause();
    }

    // -----------------------------------------------------------------
    // Admin: operator + guardian rotation
    // -----------------------------------------------------------------

    function proposeOperator(address _newOperator) external onlyOwner {
        if (_newOperator == address(0)) revert ZeroAddress();
        pendingOperator = _newOperator;
        pendingOperatorEta = block.timestamp + OPERATOR_ROTATION_DELAY;
        emit OperatorProposed(_newOperator, pendingOperatorEta);
    }

    function acceptProposedOperator() external onlyOwner {
        if (pendingOperator == address(0)) revert NoPendingOperator();
        if (block.timestamp < pendingOperatorEta) revert OperatorTimelockActive();
        address previous = operator;
        operator = pendingOperator;
        pendingOperator = address(0);
        pendingOperatorEta = 0;
        emit OperatorRotated(previous, operator);
    }

    function setGuardian(address _guardian) external onlyOwner {
        if (_guardian == address(0)) revert ZeroAddress();
        address previous = guardian;
        guardian = _guardian;
        emit GuardianRotated(previous, guardian);
    }

    // -----------------------------------------------------------------
    // Admin: sequencer feed configuration
    // -----------------------------------------------------------------

    /// @notice Wire up a Chainlink L2 sequencer-uptime feed. Owner-only.
    /// Passing `address(0)` disables the check (L1 deployments).
    function setSequencerFeed(address _feed) external onlyOwner {
        sequencerUptimeFeed = IL2SequencerUptimeFeed(_feed);
        emit SequencerFeedUpdated(_feed);
    }

    function _checkSequencer() internal view {
        IL2SequencerUptimeFeed feed = sequencerUptimeFeed;
        if (address(feed) == address(0)) {
            return;
        }
        (, int256 answer,, uint256 updatedAt,) = feed.latestRoundData();
        if (answer != 0) revert SequencerDown();
        if (block.timestamp - updatedAt < SEQUENCER_GRACE_PERIOD) {
            revert SequencerGraceActive();
        }
    }

    receive() external payable {}
}
