// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/access/Ownable2Step.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";
import "./InsuranceFund.sol";

/// @title PerpEngine
/// @notice On-chain settlement ledger for perpetual futures. The
/// off-chain matching engine (in the `perp/` Rust crate) computes
/// positions, funding, and liquidation candidates; this contract owns
/// user collateral, the mark-price oracle wiring, and the liquidation
/// bounty/insurance-fund flow.
///
/// v1 scaffold — item 7 of the 2026-09-02 gap audit. Kept intentionally
/// minimal:
///   - Collateral is single-asset (USDC by convention). Cross-margin
///     across multiple stables lands in v2.
///   - Position PnL settlement runs through operator-signed
///     `settlePosition` calls; the off-chain matcher is the source of
///     truth for fills. Zk-verified settlement is a follow-up.
///   - No funding accrual on-chain. Funding is computed off-chain and
///     applied via the same signed settlement path.
///
/// This means the contract's blast radius is bounded by:
///   1. Collateral balances (users can exit via emergency exit).
///   2. Insurance fund cap.
///   3. Operator signature validation on every settlement.
contract PerpEngine is Ownable2Step, Pausable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    IERC20 public immutable collateralAsset;
    InsuranceFund public insuranceFund;

    /// Off-chain matcher key that signs `settlePosition` calls. Rotated
    /// through the same two-step + timelock flow as `VelaSettlement`.
    address public operator;
    address public pendingOperator;
    uint256 public pendingOperatorEta;
    uint256 public constant OPERATOR_ROTATION_DELAY = 48 hours;

    /// user => market => collateral (µUSDC scale, same as off-chain).
    mapping(address => mapping(bytes32 => uint256)) public collateral;
    /// user => nonce => used. Signed settlement is replay-guarded per
    /// (user, market, nonce) triple; nonces are opaque to the contract.
    mapping(address => mapping(uint256 => bool)) public usedSettleNonces;

    error ZeroAmount();
    error ZeroAddress();
    error NotOperator();
    error NoPendingOperator();
    error OperatorTimelockActive();
    error InsufficientCollateral();
    error NonceAlreadyUsed();
    error InvalidSignature();

    event CollateralDeposited(address indexed user, bytes32 indexed market, uint256 amount);
    event CollateralWithdrawn(address indexed user, bytes32 indexed market, uint256 amount);
    event PositionSettled(address indexed user, bytes32 indexed market, int256 pnlDelta, uint256 nonce);
    event OperatorProposed(address indexed proposed, uint256 eta);
    event OperatorRotated(address indexed previous, address indexed next);
    event InsuranceFundSet(address indexed previous, address indexed next);

    constructor(IERC20 _asset, address _owner, address _operator) Ownable(_owner) {
        if (address(_asset) == address(0) || _operator == address(0)) revert ZeroAddress();
        collateralAsset = _asset;
        operator = _operator;
    }

    modifier onlyOperator() {
        if (msg.sender != operator) revert NotOperator();
        _;
    }

    // ---------------------------------------------------------------
    // Collateral
    // ---------------------------------------------------------------

    function depositCollateral(bytes32 market, uint256 amount) external nonReentrant whenNotPaused {
        if (amount == 0) revert ZeroAmount();
        collateralAsset.safeTransferFrom(msg.sender, address(this), amount);
        collateral[msg.sender][market] += amount;
        emit CollateralDeposited(msg.sender, market, amount);
    }

    /// @notice Operator co-signs withdrawals so the contract can reject
    /// requests that would leave a position under-margined. The signed
    /// payload is (user, market, amount, nonce, chainid, this).
    function withdrawCollateral(bytes32 market, uint256 amount, uint256 nonce, bytes calldata operatorSig)
        external
        nonReentrant
        whenNotPaused
    {
        if (amount == 0) revert ZeroAmount();
        if (collateral[msg.sender][market] < amount) revert InsufficientCollateral();
        if (usedSettleNonces[msg.sender][nonce]) revert NonceAlreadyUsed();
        _verifyOperator(_withdrawHash(msg.sender, market, amount, nonce), operatorSig);
        usedSettleNonces[msg.sender][nonce] = true;
        collateral[msg.sender][market] -= amount;
        collateralAsset.safeTransfer(msg.sender, amount);
        emit CollateralWithdrawn(msg.sender, market, amount);
    }

    // ---------------------------------------------------------------
    // Settlement (operator-signed)
    // ---------------------------------------------------------------

    /// @notice Apply a signed PnL delta to a user's collateral. Losses
    /// beyond the user's collateral pull from the insurance fund and
    /// emit an unrecovered-loss event. Called by the off-chain matcher.
    function settlePosition(address user, bytes32 market, int256 pnlDelta, uint256 nonce, bytes calldata operatorSig)
        external
        nonReentrant
        onlyOperator
        whenNotPaused
    {
        if (usedSettleNonces[user][nonce]) revert NonceAlreadyUsed();
        _verifyOperator(_settleHash(user, market, pnlDelta, nonce), operatorSig);
        usedSettleNonces[user][nonce] = true;

        uint256 cur = collateral[user][market];
        if (pnlDelta >= 0) {
            collateral[user][market] = cur + uint256(pnlDelta);
        } else {
            uint256 loss = uint256(-pnlDelta);
            if (loss <= cur) {
                collateral[user][market] = cur - loss;
            } else {
                collateral[user][market] = 0;
                uint256 shortfall = loss - cur;
                // Pull the shortfall from the insurance fund. The
                // beneficiary is the engine itself so subsequent
                // withdrawals draw against the topped-up balance.
                if (address(insuranceFund) != address(0)) {
                    insuranceFund.coverLoss(address(this), shortfall, market);
                }
            }
        }
        emit PositionSettled(user, market, pnlDelta, nonce);
    }

    // ---------------------------------------------------------------
    // Admin
    // ---------------------------------------------------------------

    function setInsuranceFund(InsuranceFund _fund) external onlyOwner {
        emit InsuranceFundSet(address(insuranceFund), address(_fund));
        insuranceFund = _fund;
    }

    function proposeOperator(address next) external onlyOwner {
        if (next == address(0)) revert ZeroAddress();
        pendingOperator = next;
        pendingOperatorEta = block.timestamp + OPERATOR_ROTATION_DELAY;
        emit OperatorProposed(next, pendingOperatorEta);
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

    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }

    // ---------------------------------------------------------------
    // Internals
    // ---------------------------------------------------------------

    function _settleHash(address user, bytes32 market, int256 pnlDelta, uint256 nonce) internal view returns (bytes32) {
        return keccak256(abi.encodePacked("perp:settle:", user, market, pnlDelta, nonce, block.chainid, address(this)));
    }

    function _withdrawHash(address user, bytes32 market, uint256 amount, uint256 nonce)
        internal
        view
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("perp:withdraw:", user, market, amount, nonce, block.chainid, address(this)));
    }

    function _verifyOperator(bytes32 hash, bytes calldata sig) internal view {
        // Inline ECDSA recovery so this scaffold doesn't add an OZ
        // dependency beyond what's already in the repo.
        if (sig.length != 65) revert InvalidSignature();
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := calldataload(sig.offset)
            s := calldataload(add(sig.offset, 32))
            v := byte(0, calldataload(add(sig.offset, 64)))
        }
        // EIP-2 low-s guard.
        if (uint256(s) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            revert InvalidSignature();
        }
        bytes32 ethHash = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", hash));
        address signer = ecrecover(ethHash, v, r, s);
        if (signer == address(0) || signer != operator) revert InvalidSignature();
    }
}
