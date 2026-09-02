// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "@openzeppelin/contracts/token/ERC20/extensions/ERC4626.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/// @title VLPVault
/// @notice ERC-4626 vault used by the Vela liquidity-provider program.
/// LPs deposit the underlying stablecoin and receive VLP shares; the
/// authorised strategist (typically the perp engine or an MM operator)
/// can rebalance up to `strategistPullCap` in a single call.
///
/// Scaffold for item 7 / BUILDPLAN Tier 2 "MM credit vault". Simplified
/// vs a full v1:
///   - Single strategist address (no allowlist).
///   - Linear share accounting via OZ ERC4626 — no fee curves yet.
///   - Withdrawals honoured immediately (no unbonding queue).
///
/// Hardening a v2 needs before mainnet:
///   - Time-weighted deposit lock to prevent flash-loan share dilution.
///   - Withdrawal queue with per-epoch limits.
///   - Explicit share inflation guard (donation-attack protection).
contract VLPVault is ERC4626, Ownable, Pausable {
    using SafeERC20 for IERC20;

    address public strategist;
    /// Max notional the strategist may pull in a single `strategistPull`
    /// call. Bounds the blast radius of a strategist-key compromise.
    uint256 public strategistPullCap;

    error NotStrategist();
    error ZeroAddress();
    error PullCapExceeded();
    error ZeroAmount();

    event StrategistUpdated(address indexed previous, address indexed next);
    event StrategistPullCapUpdated(uint256 previous, uint256 next);
    event StrategistPulled(address indexed to, uint256 amount);
    event StrategistReturned(address indexed from, uint256 amount);

    constructor(IERC20 _asset, string memory _name, string memory _symbol, address _owner)
        ERC20(_name, _symbol)
        ERC4626(_asset)
        Ownable(_owner)
    {
        strategistPullCap = 0;
    }

    modifier onlyStrategist() {
        if (msg.sender != strategist) revert NotStrategist();
        _;
    }

    // ---------------------------------------------------------------
    // Admin
    // ---------------------------------------------------------------

    function setStrategist(address _strategist) external onlyOwner {
        if (_strategist == address(0)) revert ZeroAddress();
        emit StrategistUpdated(strategist, _strategist);
        strategist = _strategist;
    }

    function setStrategistPullCap(uint256 _cap) external onlyOwner {
        emit StrategistPullCapUpdated(strategistPullCap, _cap);
        strategistPullCap = _cap;
    }

    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }

    // ---------------------------------------------------------------
    // Strategist push / pull
    // ---------------------------------------------------------------

    /// @notice Strategist withdraws underlying to execute strategy.
    /// Bounded by `strategistPullCap` and disabled while paused.
    function strategistPull(uint256 amount) external onlyStrategist whenNotPaused {
        if (amount == 0) revert ZeroAmount();
        if (amount > strategistPullCap) revert PullCapExceeded();
        IERC20(asset()).safeTransfer(strategist, amount);
        emit StrategistPulled(strategist, amount);
    }

    /// @notice Strategist returns underlying (PnL or repayment). No
    /// permission gate on returns — anyone can top the vault up.
    function strategistReturn(uint256 amount) external {
        if (amount == 0) revert ZeroAmount();
        IERC20(asset()).safeTransferFrom(msg.sender, address(this), amount);
        emit StrategistReturned(msg.sender, amount);
    }

    // ---------------------------------------------------------------
    // ERC4626 overrides that respect the pause switch
    // ---------------------------------------------------------------

    function deposit(uint256 assets, address receiver)
        public
        override
        whenNotPaused
        returns (uint256)
    {
        return super.deposit(assets, receiver);
    }

    function mint(uint256 shares, address receiver)
        public
        override
        whenNotPaused
        returns (uint256)
    {
        return super.mint(shares, receiver);
    }

    function withdraw(uint256 assets, address receiver, address owner_)
        public
        override
        whenNotPaused
        returns (uint256)
    {
        return super.withdraw(assets, receiver, owner_);
    }

    function redeem(uint256 shares, address receiver, address owner_)
        public
        override
        whenNotPaused
        returns (uint256)
    {
        return super.redeem(shares, receiver, owner_);
    }
}
