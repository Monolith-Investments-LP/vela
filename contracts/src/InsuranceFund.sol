// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title InsuranceFund
/// @notice On-chain reserve that backs perp liquidations. Anyone can
/// deposit USDC; only the authorised perp engine can drain funds to
/// cover bad-debt losses. Withdrawals by depositors are not supported
/// in v1 — the fund is a one-way sink until governance drains it.
///
/// This is the P0 scaffold called out in item 7 of the 2026-09-02 gap
/// audit. It intentionally omits:
///   - Per-depositor accounting (no share tokens).
///   - Withdrawal path other than governance drain.
///   - Interest / yield.
/// A v2 adds ERC4626 accounting and a socialised-loss path.
contract InsuranceFund is Ownable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    IERC20 public immutable asset;
    address public perpEngine;

    error NotPerpEngine();
    error ZeroAmount();
    error ZeroAddress();
    error InsufficientReserve();

    event Deposited(address indexed from, uint256 amount);
    event LossCovered(address indexed to, uint256 amount, bytes32 indexed reason);
    event PerpEngineUpdated(address indexed previous, address indexed next);
    event GovernanceDrained(address indexed to, uint256 amount);

    constructor(IERC20 _asset, address _owner) Ownable(_owner) {
        if (address(_asset) == address(0)) revert ZeroAddress();
        asset = _asset;
    }

    modifier onlyPerpEngine() {
        if (msg.sender != perpEngine) revert NotPerpEngine();
        _;
    }

    /// @notice Anyone can top up the fund.
    function deposit(uint256 amount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        asset.safeTransferFrom(msg.sender, address(this), amount);
        emit Deposited(msg.sender, amount);
    }

    /// @notice Wire (or rotate) the perp engine that may pull losses.
    /// Owner-only. Zero address deauthorises pulls entirely.
    function setPerpEngine(address _engine) external onlyOwner {
        emit PerpEngineUpdated(perpEngine, _engine);
        perpEngine = _engine;
    }

    /// @notice Perp engine calls this when a liquidation leaves bad
    /// debt that the borrower's collateral can't cover.
    function coverLoss(address to, uint256 amount, bytes32 reason)
        external
        nonReentrant
        onlyPerpEngine
    {
        if (amount == 0) revert ZeroAmount();
        if (to == address(0)) revert ZeroAddress();
        if (asset.balanceOf(address(this)) < amount) revert InsufficientReserve();
        asset.safeTransfer(to, amount);
        emit LossCovered(to, amount, reason);
    }

    /// @notice Emergency drain by governance. Kept explicit so a
    /// depositor can trace where funds went — no silent sweeps.
    function governanceDrain(address to, uint256 amount) external onlyOwner nonReentrant {
        if (amount == 0) revert ZeroAmount();
        if (to == address(0)) revert ZeroAddress();
        asset.safeTransfer(to, amount);
        emit GovernanceDrained(to, amount);
    }

    function reserveBalance() external view returns (uint256) {
        return asset.balanceOf(address(this));
    }
}
