// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";
import "../src/InsuranceFund.sol";
import "../src/VLPVault.sol";
import "../src/SequencerRegistry.sol";
import "../src/PerpEngine.sol";

contract MockUsdc is ERC20 {
    constructor() ERC20("USDC", "USDC") {
        _mint(msg.sender, 10_000_000e6);
    }

    function decimals() public pure override returns (uint8) {
        return 6;
    }
}

// -------------------------------------------------------------------
// InsuranceFund
// -------------------------------------------------------------------

contract InsuranceFundTest is Test {
    MockUsdc usdc;
    InsuranceFund fund;
    address engine = address(0xE0);
    address lp = address(0xA1);

    function setUp() public {
        usdc = new MockUsdc();
        fund = new InsuranceFund(usdc, address(this));
        fund.setPerpEngine(engine);
        usdc.transfer(lp, 100_000e6);
    }

    function test_depositAndCover() public {
        vm.startPrank(lp);
        usdc.approve(address(fund), 50_000e6);
        fund.deposit(50_000e6);
        vm.stopPrank();
        assertEq(fund.reserveBalance(), 50_000e6);

        vm.prank(engine);
        fund.coverLoss(engine, 1_000e6, bytes32("BTC-PERP"));
        assertEq(usdc.balanceOf(engine), 1_000e6);
        assertEq(fund.reserveBalance(), 49_000e6);
    }

    function test_coverLossRejectsNonEngine() public {
        vm.expectRevert(InsuranceFund.NotPerpEngine.selector);
        fund.coverLoss(address(this), 1, bytes32("nope"));
    }

    function test_governanceDrain() public {
        vm.startPrank(lp);
        usdc.approve(address(fund), 10e6);
        fund.deposit(10e6);
        vm.stopPrank();
        fund.governanceDrain(address(this), 10e6);
        assertEq(usdc.balanceOf(address(this)), 10_000_000e6 - 100_000e6 + 10e6);
    }
}

// -------------------------------------------------------------------
// VLPVault
// -------------------------------------------------------------------

contract VLPVaultTest is Test {
    MockUsdc usdc;
    VLPVault vault;
    address lp = address(0xA2);
    address strategist = address(0xB1);

    function setUp() public {
        usdc = new MockUsdc();
        vault = new VLPVault(usdc, "Vela LP", "VLP", address(this));
        vault.setStrategist(strategist);
        vault.setStrategistPullCap(500_000e6);
        usdc.transfer(lp, 100_000e6);
    }

    function test_depositMintsShares() public {
        vm.startPrank(lp);
        usdc.approve(address(vault), 10_000e6);
        vault.deposit(10_000e6, lp);
        vm.stopPrank();
        assertGt(vault.balanceOf(lp), 0);
        assertEq(vault.totalAssets(), 10_000e6);
    }

    function test_strategistPullBounded() public {
        vm.startPrank(lp);
        usdc.approve(address(vault), 100_000e6);
        vault.deposit(100_000e6, lp);
        vm.stopPrank();

        vm.prank(strategist);
        vm.expectRevert(VLPVault.PullCapExceeded.selector);
        vault.strategistPull(600_000e6);

        vm.prank(strategist);
        vault.strategistPull(50_000e6);
        assertEq(usdc.balanceOf(strategist), 50_000e6);
    }

    function test_pauseBlocksDeposits() public {
        vault.pause();
        vm.startPrank(lp);
        usdc.approve(address(vault), 10e6);
        vm.expectRevert(Pausable.EnforcedPause.selector);
        vault.deposit(10e6, lp);
        vm.stopPrank();
    }
}

// -------------------------------------------------------------------
// SequencerRegistry
// -------------------------------------------------------------------

contract SequencerRegistryTest is Test {
    MockUsdc usdc;
    SequencerRegistry reg;
    address seq = address(0xC1);
    address beneficiary = address(0xD1);

    function setUp() public {
        usdc = new MockUsdc();
        reg = new SequencerRegistry(usdc, 10_000e6, address(this));
        usdc.transfer(seq, 100_000e6);
    }

    function test_registerAndWithdrawAfterUnbond() public {
        vm.startPrank(seq);
        usdc.approve(address(reg), 20_000e6);
        reg.register(20_000e6);
        vm.stopPrank();
        assertTrue(reg.isActive(seq));

        vm.prank(seq);
        reg.initiateUnbond();

        vm.warp(block.timestamp + 7 days);
        vm.prank(seq);
        reg.withdraw();
        assertEq(usdc.balanceOf(seq), 100_000e6);
        assertFalse(reg.isActive(seq));
    }

    function test_registerBelowMinReverts() public {
        vm.startPrank(seq);
        usdc.approve(address(reg), 1_000e6);
        vm.expectRevert(SequencerRegistry.BondBelowMinimum.selector);
        reg.register(1_000e6);
        vm.stopPrank();
    }

    function test_slashRoutesToBeneficiary() public {
        vm.startPrank(seq);
        usdc.approve(address(reg), 20_000e6);
        reg.register(20_000e6);
        vm.stopPrank();

        reg.slash(seq, 5_000e6, keccak256("withheld"), beneficiary);
        assertEq(usdc.balanceOf(beneficiary), 5_000e6);
        (uint256 bond, uint256 slashed,,) = reg.sequencers(seq);
        assertEq(bond, 15_000e6);
        assertEq(slashed, 5_000e6);
    }
}

// -------------------------------------------------------------------
// PerpEngine — collateral deposit + operator-signed settle round-trip
// -------------------------------------------------------------------

contract PerpEngineTest is Test {
    MockUsdc usdc;
    InsuranceFund fund;
    PerpEngine engine;
    address user = address(0xE1);

    uint256 opKey = 0xB0B;
    address op;

    function setUp() public {
        op = vm.addr(opKey);
        usdc = new MockUsdc();
        fund = new InsuranceFund(usdc, address(this));
        engine = new PerpEngine(usdc, address(this), op);
        engine.setInsuranceFund(fund);
        fund.setPerpEngine(address(engine));
        usdc.transfer(user, 10_000e6);
    }

    function _settleSig(address u, bytes32 m, int256 pnl, uint256 nonce) internal view returns (bytes memory) {
        bytes32 h = keccak256(
            abi.encodePacked("perp:settle:", u, m, pnl, nonce, block.chainid, address(engine))
        );
        bytes32 eth = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", h));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(opKey, eth);
        return abi.encodePacked(r, s, v);
    }

    function test_depositAndSettlePositive() public {
        vm.startPrank(user);
        usdc.approve(address(engine), 1_000e6);
        engine.depositCollateral(bytes32("BTC-PERP"), 1_000e6);
        vm.stopPrank();
        assertEq(engine.collateral(user, bytes32("BTC-PERP")), 1_000e6);

        bytes memory sig = _settleSig(user, bytes32("BTC-PERP"), 200e6, 1);
        vm.prank(op);
        engine.settlePosition(user, bytes32("BTC-PERP"), 200e6, 1, sig);
        assertEq(engine.collateral(user, bytes32("BTC-PERP")), 1_200e6);
    }

    function test_settleReplayRejected() public {
        vm.startPrank(user);
        usdc.approve(address(engine), 100e6);
        engine.depositCollateral(bytes32("ETH-PERP"), 100e6);
        vm.stopPrank();
        bytes memory sig = _settleSig(user, bytes32("ETH-PERP"), 10e6, 7);
        vm.startPrank(op);
        engine.settlePosition(user, bytes32("ETH-PERP"), 10e6, 7, sig);
        vm.expectRevert(PerpEngine.NonceAlreadyUsed.selector);
        engine.settlePosition(user, bytes32("ETH-PERP"), 10e6, 7, sig);
        vm.stopPrank();
    }

    function test_operatorRotationTimelock() public {
        address next = address(0xF00D);
        engine.proposeOperator(next);
        vm.expectRevert(PerpEngine.OperatorTimelockActive.selector);
        engine.acceptProposedOperator();
        vm.warp(block.timestamp + 48 hours);
        engine.acceptProposedOperator();
        assertEq(engine.operator(), next);
    }
}
