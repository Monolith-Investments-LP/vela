// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title SequencerRegistry
/// @notice On-chain bond registry for the rotating-sequencer program
/// described in BUILDPLAN Tier 5. Each sequencer posts a bond, is
/// eligible to be elected for an epoch, and can be slashed by
/// governance for withholding decryption shares or double-signing.
///
/// v1 scaffold — the elected-sequencer selection algorithm and
/// leader-rotation VRF are follow-ups; this contract only owns the
/// bond ledger and slashing hook.
///
/// Storage layout is deliberately conservative: everything is
/// address-keyed, no arrays, so a compromised owner can't grief via
/// unbounded loops.
contract SequencerRegistry is Ownable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    IERC20 public immutable bondAsset;
    uint256 public immutable minBond;
    uint256 public constant UNBOND_DELAY = 7 days;

    struct SequencerRecord {
        uint256 bond;
        uint256 slashedTotal;
        uint256 unbondEligibleAt;
        bool active;
    }

    mapping(address => SequencerRecord) public sequencers;
    uint256 public totalBonded;
    uint256 public totalSlashed;

    error BondBelowMinimum();
    error NotRegistered();
    error UnbondingNotElapsed();
    error ZeroAmount();
    error ZeroAddress();
    error AlreadyActive();

    event Registered(address indexed sequencer, uint256 bond);
    event BondIncreased(address indexed sequencer, uint256 amount, uint256 newBond);
    event UnbondInitiated(address indexed sequencer, uint256 eligibleAt);
    event Withdrawn(address indexed sequencer, uint256 amount);
    event Slashed(address indexed sequencer, uint256 amount, bytes32 indexed reason, address indexed beneficiary);

    constructor(IERC20 _bondAsset, uint256 _minBond, address _owner) Ownable(_owner) {
        if (address(_bondAsset) == address(0)) revert ZeroAddress();
        bondAsset = _bondAsset;
        minBond = _minBond;
    }

    // ---------------------------------------------------------------
    // Sequencer-side actions
    // ---------------------------------------------------------------

    function register(uint256 bond) external nonReentrant {
        if (bond < minBond) revert BondBelowMinimum();
        SequencerRecord storage rec = sequencers[msg.sender];
        if (rec.active) revert AlreadyActive();
        bondAsset.safeTransferFrom(msg.sender, address(this), bond);
        rec.bond = bond;
        rec.active = true;
        rec.unbondEligibleAt = 0;
        totalBonded += bond;
        emit Registered(msg.sender, bond);
    }

    function topUp(uint256 amount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        SequencerRecord storage rec = sequencers[msg.sender];
        if (!rec.active) revert NotRegistered();
        bondAsset.safeTransferFrom(msg.sender, address(this), amount);
        rec.bond += amount;
        totalBonded += amount;
        emit BondIncreased(msg.sender, amount, rec.bond);
    }

    function initiateUnbond() external {
        SequencerRecord storage rec = sequencers[msg.sender];
        if (!rec.active) revert NotRegistered();
        rec.unbondEligibleAt = block.timestamp + UNBOND_DELAY;
        emit UnbondInitiated(msg.sender, rec.unbondEligibleAt);
    }

    function withdraw() external nonReentrant {
        SequencerRecord storage rec = sequencers[msg.sender];
        if (!rec.active) revert NotRegistered();
        if (rec.unbondEligibleAt == 0 || block.timestamp < rec.unbondEligibleAt) {
            revert UnbondingNotElapsed();
        }
        uint256 amount = rec.bond;
        rec.bond = 0;
        rec.active = false;
        rec.unbondEligibleAt = 0;
        totalBonded -= amount;
        bondAsset.safeTransfer(msg.sender, amount);
        emit Withdrawn(msg.sender, amount);
    }

    // ---------------------------------------------------------------
    // Owner / governance actions
    // ---------------------------------------------------------------

    /// @notice Slash a registered sequencer. `beneficiary` receives the
    /// slashed bond — typically an insurance fund or protocol treasury.
    /// `reason` is a caller-supplied tag surfaced in events (e.g.
    /// keccak256("withheld-share")) for offline attribution.
    function slash(address sequencer, uint256 amount, bytes32 reason, address beneficiary)
        external
        onlyOwner
        nonReentrant
    {
        if (beneficiary == address(0)) revert ZeroAddress();
        SequencerRecord storage rec = sequencers[sequencer];
        if (!rec.active) revert NotRegistered();
        uint256 cap = rec.bond;
        uint256 applied = amount > cap ? cap : amount;
        rec.bond -= applied;
        rec.slashedTotal += applied;
        totalBonded -= applied;
        totalSlashed += applied;
        bondAsset.safeTransfer(beneficiary, applied);
        emit Slashed(sequencer, applied, reason, beneficiary);
    }

    function isActive(address sequencer) external view returns (bool) {
        return sequencers[sequencer].active;
    }
}
