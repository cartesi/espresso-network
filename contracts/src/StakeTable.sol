pragma solidity ^0.8.0;

import { SafeTransferLib, ERC20 } from "solmate/utils/SafeTransferLib.sol";
import { OwnableUpgradeable } from
    "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import { Initializable } from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import { UUPSUpgradeable } from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import { BN254 } from "bn254/BN254.sol";
import { BLSSig } from "./libraries/BLSSig.sol";
import { LightClient } from "../src/LightClient.sol";
import { EdOnBN254 } from "./libraries/EdOnBn254.sol";
import { InitializedAt } from "./InitializedAt.sol";

using EdOnBN254 for EdOnBN254.EdOnBN254Point;

/// @title Ethereum L1 component of the Espresso Global Confirmation Layer (GCL) stake table.
///
/// @dev All functions are marked as virtual so that future upgrades can override them.
contract StakeTable is Initializable, InitializedAt, OwnableUpgradeable, UUPSUpgradeable {
    // === Events ===

    /// @notice upgrade event when the proxy updates the implementation it's pointing to

    // TODO: is this event useful, it currently emits the same data as the UUPSUpgradeable Upgraded
    // event. Consider making it more useful or removing it.
    event Upgrade(address implementation);

    /// @notice A registration of a new validator.
    ///
    /// @notice Signals to the confirmation layer that a new validator is ready to receive
    /// delegations in the stake table contract. The confirmation layer uses this event to keep
    /// track of the validator's keys for the stake table.
    ///
    /// @notice The commission is in % with 2 decimals, from 0.00% (value 0) to 100% (value 10_000).
    ///
    /// @notice A validator registration is only valid if the BLS and Schnorr signature are valid.
    /// The GCL must verify this and otherwise discard the validator registration when it processes
    /// the event. The contract cannot verify the validity of the registration event and delegators
    /// will be able to deposit as soon as this event is emitted. In the event that a delegator
    /// delegates to an invalid validator the delegator can withdraw the delegation again in the
    /// same way they can withdraw other delegations.
    ///
    /// @notice UIs should do their best to prevent invalid, or duplicate registrations.
    ///
    /// @notice The verification key of the BLS keypair used for consensus signing is a
    /// `BN254.G2Point`.
    ///
    /// @notice The verification key of the state signing schnorr keypair is an
    /// `EdOnBN254.EdOnBN254Point`.
    event ValidatorRegistered(
        address indexed account,
        BN254.G2Point blsVk,
        EdOnBN254.EdOnBN254Point schnorrVk,
        uint16 commission
    );
    // TODO: emit the BLS signature so GCL can verify it.
    // TODO: emit the Schnorr signature so GCL can verify it.

    /// @notice A validator initiated an exit from stake table
    ///
    /// @notice All funds delegated to this validator are marked for withdrawal. Users can no longer
    /// delegate to this validator. Their previously delegated funds are automatically undelegated.
    /// After `exitEscrowPeriod` elapsed, delegators can claim the funds delegated to the exited
    /// validator via `claimValidatorExit`.
    ///
    /// @notice The GCL removes this validator and all its delegations from the active validator
    /// set.
    event ValidatorExit(address indexed validator);

    /// @notice A Delegator delegated funds to a validator.
    ///
    /// @notice The tokens are transferred to the stake table contract.
    ///
    /// @notice The GCL adjusts the weight for this validator and the delegators delegation
    /// associated with it.
    event Delegated(address indexed delegator, address indexed validator, uint256 amount);

    /// @notice A delegator undelegation funds from a validator.
    ///
    /// @notice The tokens are marked to be unlocked for withdrawal.
    ///
    /// @notice The GCL needs to update the stake table and adjust the weight for this validator and
    /// the delegators delegation associated with it.
    event Undelegated(address indexed delegator, address indexed validator, uint256 amount);

    /// @notice A validator updates their signing keys.
    ///
    /// @notice Similarly to registration events, the correctness cannot be fully determined by the
    /// contracts.
    ///
    /// @notice The confirmation layer needs to update the stake table with the new keys.
    event ConsensusKeysUpdated(
        address indexed account, BN254.G2Point blsVK, EdOnBN254.EdOnBN254Point schnorrVK
    );
    // TODO: emit the BLS signature so GCL can verify it.
    // TODO: emit the Schnorr signature so GCL can verify it.

    /// @notice A delegator claims unlocked funds.
    ///
    /// @notice This event is not relevant for the GCL. The events that remove stake from the stake
    /// table are `Undelegated` and `ValidatorExit`.
    event Withdrawal(address indexed account, uint256 amount);

    // === Errors ===

    /// A user tries to register a validator with the same address
    error ValidatorAlreadyRegistered();

    //// A validator is not active.
    error ValidatorInactive();

    /// A validator has already exited.
    error ValidatorAlreadyExited();

    /// A validator has not exited yet.
    error ValidatorNotExited();

    // A user tries to withdraw funds before the exit escrow period is over.
    error PrematureWithdrawal();

    // This contract does not have the sufficient allowance on the staking asset.
    error InsufficientAllowance(uint256, uint256);

    // The delegator does not have the sufficient staking asset balance to delegate.
    error InsufficientBalance(uint256);

    // A delegator does not have the sufficient balance to withdraw.
    error NothingToWithdraw();

    // A validator provides a zero SchnorrVK.
    error InvalidSchnorrVK();

    /// The BLS key has been previously registered in the contract.
    error BlsKeyAlreadyUsed();

    /// The commission value is invalid.
    error InvalidCommission();

    /// Contract dependencies initialized with zero address.
    error ZeroAddress();

    // === Structs ===

    /// @notice Represents an Espresso validator and tracks funds currently delegated to them.
    ///
    /// @notice The `delegatedAmount` excludes funds that are currently marked for withdrawal via
    /// undelegation or validator exit.
    struct Validator {
        uint256 delegatedAmount;
        ValidatorStatus status;
    }

    /// @notice The status of a validator.
    ///
    /// By default a validator is in the `Unknown` state. This means it has never registered. Upon
    /// registration the status will become `Active` and if the validator deregisters its status
    /// becomes `Exited`.
    enum ValidatorStatus {
        Unknown,
        Active,
        Exited
    }

    /// @notice Tracks an undelegation from a validator.
    struct Undelegation {
        uint256 amount;
        uint256 unlocksAt;
    }

    // === Storage ===

    /// @notice Reference to the light client contract.
    ///
    /// @dev Currently unused but will be used for slashing therefore already included in the
    /// contract.
    LightClient public lightClient;

    /// The staking token contract.
    ERC20 public token;

    /// @notice All validators the contract knows about.
    mapping(address account => Validator validator) public validators;

    /// BLS keys that have been seen by the contract
    ///
    /// @dev to simplify the reasoning about what keys and prevent some errors due to
    /// misconfigurations of validators the contract currently marks keys as used and only allow
    /// them to be used once. This for example prevents callers from accidentally registering the
    /// same BLS key twice.
    mapping(bytes32 blsKeyHash => bool used) public blsKeys;

    /// Validators that have exited and the time at which delegators can claim their funds.
    mapping(address validator => uint256 unlocksAt) public validatorExits;

    /// Currently active delegation amounts.
    mapping(address validator => mapping(address delegator => uint256 amount)) public delegations;

    /// Delegations held in escrow that are to be unlocked at a later time.
    //
    // @dev these are stored indexed by validator so we can keep track of them for slashing later
    mapping(address validator => mapping(address delegator => Undelegation)) undelegations;

    /// The time the contract will hold funds after undelegations are requested.
    ///
    /// Must allow ample time for node to exit active validator set and slashing
    /// evidence to be submitted.
    uint256 public exitEscrowPeriod;

    /// @notice since the constructor initializes storage on this contract we disable it
    /// @dev storage is on the proxy contract since it calls this contract via delegatecall
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    function initialize(
        address _tokenAddress,
        address _lightClientAddress,
        uint256 _exitEscrowPeriod,
        address _initialOwner
    ) public initializer {
        __Ownable_init(_initialOwner);
        __UUPSUpgradeable_init();
        initializeAtBlock();

        initializeState(_tokenAddress, _lightClientAddress, _exitEscrowPeriod);
    }

    function initializeState(
        address _tokenAddress,
        address _lightClientAddress,
        uint256 _exitEscrowPeriod
    ) internal {
        if (_tokenAddress == address(0)) {
            revert ZeroAddress();
        }
        if (_lightClientAddress == address(0)) {
            revert ZeroAddress();
        }
        token = ERC20(_tokenAddress);
        lightClient = LightClient(_lightClientAddress);
        // @audit - no bounds on exitEscrowPeriod
        exitEscrowPeriod = _exitEscrowPeriod;
    }

    /// @notice Use this to get the implementation contract version
    /// @return majorVersion The major version of the contract
    /// @return minorVersion The minor version of the contract
    /// @return patchVersion The patch version of the contract
    function getVersion()
        public
        pure
        virtual
        returns (uint8 majorVersion, uint8 minorVersion, uint8 patchVersion)
    {
        return (1, 0, 0);
    }

    /// @notice only the owner can authorize an upgrade
    function _authorizeUpgrade(address newImplementation) internal virtual override onlyOwner {
        emit Upgrade(newImplementation);
    }

    /// @dev Computes a hash value of some G2 point.
    /// @param blsVK BLS verification key in G2
    /// @return keccak256(blsVK)
    function _hashBlsKey(BN254.G2Point memory blsVK) public pure returns (bytes32) {
        return keccak256(abi.encode(blsVK.x0, blsVK.x1, blsVK.y0, blsVK.y1));
    }

    function ensureValidatorActive(address validator) internal view {
        if (!(validators[validator].status == ValidatorStatus.Active)) {
            revert ValidatorInactive();
        }
    }

    function ensureValidatorNotRegistered(address validator) internal view {
        // @audit-issue - This means that someone cannot re-join if they decide to stop being here
        if (validators[validator].status != ValidatorStatus.Unknown) {
            revert ValidatorAlreadyRegistered();
        }
    }

    // @audit-issue - this check weather a validator is planning to exit/ has exited ie if a validator has at any point said I want to exit
    function ensureValidatorNotExited(address validator) internal view {
        if (validatorExits[validator] != 0) {
            revert ValidatorAlreadyExited();
        }
    }

    function ensureNewKey(BN254.G2Point memory blsVK) internal view {
        if (blsKeys[_hashBlsKey(blsVK)]) {
            revert BlsKeyAlreadyUsed();
        }
    }

    // @dev We don't check the validity of the schnorr verifying key but providing a zero key is
    // definitely a mistake by the caller, therefore we revert.
    function ensureNonZeroSchnorrKey(EdOnBN254.EdOnBN254Point memory schnorrVK) internal pure {
        EdOnBN254.EdOnBN254Point memory zeroSchnorrKey = EdOnBN254.EdOnBN254Point(0, 0);

        if (schnorrVK.isEqual(zeroSchnorrKey)) {
            revert InvalidSchnorrVK();
        }
    }

    /// @notice Register a validator in the stake table
    ///
    /// @param blsVK The BLS verification key
    /// @param schnorrVK The Schnorr verification key (as the auxiliary info)
    /// @param blsSig The BLS signature that authenticates the ethereum account this function is
    ///        called from
    /// @param commission in % with 2 decimals, from 0.00% (value 0) to 100% (value 10_000)
    ///
    /// @notice The function will revert if
    ///
    ///      1) the validator is already registered
    ///      2) the schnorr key is zero
    ///      3) if the bls signature verification fails (this prevents rogue public-key attacks).
    ///      4) the commission is > 100%
    ///
    /// @notice No validity check on `schnorrVK` due to gas cost of Rescue hash, UIs should perform
    /// checks where possible and alert users.
    function registerValidator(
        BN254.G2Point memory blsVK,
        EdOnBN254.EdOnBN254Point memory schnorrVK,
        BN254.G1Point memory blsSig,
        uint16 commission
    ) external virtual {
        address validator = msg.sender;

        // address can not have been used before.
        // @audit - what if we exit can we enter again?
        ensureValidatorNotRegistered(validator);
        // ensure that the schnorr key is not zero
        // @audit-issue - someone could steam your schnorrKey via a front run, in could disable people from ever registering
        ensureNonZeroSchnorrKey(schnorrVK);
        // ensure that the bls key has not been used before
        ensureNewKey(blsVK);

        // Verify that the validator can sign for that blsVK. This prevents rogue public-key
        // attacks.
        //
        // TODO: we will move this check to the GCL to save gas.
        bytes memory message = abi.encode(validator);
        // since we dont have control over msg.sender/validator/message in this case we know that the signature would be validated to the sendeer
        BLSSig.verifyBlsSig(message, blsSig, blsVK);

        // @audit - there is nothing that goes into what the commission is used for?
        // @audit - should there be a lower and upper bound?
        if (commission > 10000) {
            revert InvalidCommission();
        }

        // ensures that the bls key is now used so it cant be registered again
        blsKeys[_hashBlsKey(blsVK)] = true;
        // set the validator to active
        validators[validator] = Validator({ status: ValidatorStatus.Active, delegatedAmount: 0 });

        emit ValidatorRegistered(validator, blsVK, schnorrVK, commission);
    }

    /// @notice Deregister a validator
    function deregisterValidator() external virtual {
        address validator = msg.sender;
        ensureValidatorActive(validator);

        // Q: if you deregister what happens to all the delegations?
        // A: they are still able to claim/withdraw their funds after the exit escrow period via claimValidatorExit
        // @audit-issue - if I deregister I can not go back in might be by design but it could be as simple as having it be a enum of 2 instead of 3 like
        // dead, active
        validators[validator].status = ValidatorStatus.Exited;
        validatorExits[validator] = block.timestamp + exitEscrowPeriod;

        emit ValidatorExit(validator);
    }

    /// @notice Delegate to a validator
    /// @param validator The validator to delegate to
    /// @param amount The amount to delegate
    function delegate(address validator, uint256 amount) external virtual {
        ensureValidatorActive(validator);
        address delegator = msg.sender;

        // TODO: revert if amount is zero
        uint256 allowance = token.allowance(delegator, address(this));
        if (allowance < amount) {
            revert InsufficientAllowance(allowance, amount);
        }
        // the total delegated amount is the sum of all delegations
        validators[validator].delegatedAmount += amount;
        // the amount delegated by 1 actor to this validator
        delegations[validator][delegator] += amount;

        SafeTransferLib.safeTransferFrom(token, delegator, address(this), amount);

        emit Delegated(delegator, validator, amount);
    }

    /// @notice Undelegate from a validator
    /// @param validator The validator to undelegate from
    /// @param amount The amount to undelegate
    function undelegate(address validator, uint256 amount) external virtual {
        ensureValidatorActive(validator);
        address delegator = msg.sender;

        // TODO: revert if amount is zero

        if (validators[delegator].status == ValidatorStatus.Exited) {
            revert ValidatorAlreadyExited();
        }

        uint256 balance = delegations[validator][delegator];
        if (balance < amount) {
            revert InsufficientBalance(balance);
        }

        delegations[validator][delegator] -= amount;
        // @audit-issue - if you undelegate any amount and want to undelegate more you have to wait the full escrow period again
        // ie say I want to delegate 5 and before the escrow period is over I undelegate 3, I have to wait the full escrow period before I can undelegate the remaining 2

        // @audit-issue - CRITICAL- if we have 20
        // undelegate 10 and undelegate 10 again, we will lose the first 10 since it will override the previous undelegation
        undelegations[validator][delegator] = Undelegation({ amount: amount, unlocksAt: block.timestamp + exitEscrowPeriod });

        emit Undelegated(delegator, validator, amount);
    }

    /// @notice Withdraw previously delegated funds after an undelegation.
    /// @param validator The validator to withdraw from
    function claimWithdrawal(address validator) external virtual {
        address delegator = msg.sender;
        // If entries are missing at any of the levels of the mapping this will return zero
        uint256 amount = undelegations[validator][delegator].amount;
        if (amount == 0) {
            revert NothingToWithdraw();
        }

        if (block.timestamp < undelegations[validator][delegator].unlocksAt) {
            revert PrematureWithdrawal();
        }

        // Mark funds as spent
        delete undelegations[validator][delegator];

        SafeTransferLib.safeTransfer(token, delegator, amount);

        emit Withdrawal(delegator, amount);
    }

    /// @notice Withdraw previously delegated funds after a validator has exited
    /// @param validator The validator to withdraw from
    function claimValidatorExit(address validator) external virtual {
        address delegator = msg.sender;
        uint256 unlocksAt = validatorExits[validator];
        if (unlocksAt == 0) {
            revert ValidatorNotExited();
        }

        if (block.timestamp < unlocksAt) {
            revert PrematureWithdrawal();
        }

        uint256 amount = delegations[validator][delegator];
        if (amount == 0) {
            revert NothingToWithdraw();
        }

        // @audit-issue - INF - does not delete anymore
        // Mark funds as spent
        // @audit-issue - does not delete
        delegations[validator][delegator] = 0;

        SafeTransferLib.safeTransfer(token, delegator, amount);

        emit Withdrawal(delegator, amount);
    }

    /// @notice Update the consensus keys for a validator
    /// @dev This function is used to update the consensus keys for a validator
    /// @dev This function can only be called by the validator itself when it hasn't exited
    ///      TODO: MA: is this a good idea? Why should key rotation be blocked for an exiting
    ///      validator?
    /// @dev The validator will need to give up either its old BLS key and/or old Schnorr key
    /// @dev The validator will need to provide a BLS signature to prove that the account owns the
    /// new BLS key
    /// @param newBlsVK The new BLS verification key
    /// @param newSchnorrVK The new Schnorr verification key
    /// @param newBlsSig The BLS signature that the account owns the new BLS key
    ///
    /// TODO: MA: I think this function should be reworked. Is it fine to always force updating both
    /// keys? If not we should probably rather have two functions for updating the keys. But this
    /// would also mean two separate events, or storing the keys in the contract only for this
    /// update function to remit the old keys, or throw errors if the keys are not changed. None of
    /// that seems useful enough to warrant the extra complexity in the contract and GCL.
    // @audit-issue - INF - if you update your keys you take up two slots and does not free up the key we changed from
    function updateConsensusKeys(
        BN254.G2Point memory newBlsVK,              // The new BLS verification key, passed as a struct stored in memory.
        EdOnBN254.EdOnBN254Point memory newSchnorrVK,  // The new Schnorr verification key, also a struct in memory.
        BN254.G1Point memory newBlsSig               // The new BLS signature, in memory, proving ownership of the new BLS key.
    ) external virtual {                              // Function is externally callable and can be overridden in derived contracts.
        address validator = msg.sender;             // Get the address of the caller; this should be the validator updating its keys.

        ensureValidatorActive(validator);           // Check that the validator is active (has not exited).
                                                    // If not active, the function will revert.

        ensureNonZeroSchnorrKey(newSchnorrVK);        // Ensure that the new Schnorr key is not the zero value (i.e., it is valid).

        ensureNewKey(newBlsVK);                       // Ensure that the new BLS key hasn't already been used.

        // Prepare a message for signature verification:
        // Here, we encode the validator's address into bytes so that it becomes the message that is signed.
        bytes memory message = abi.encode(validator);

        // Verify the BLS signature:
        // This function checks that the newBlsSig is a valid signature for the 'message' using the newBlsVK.
        // This step proves that the validator owns the new BLS key.
        BLSSig.verifyBlsSig(message, newBlsSig, newBlsVK);

        // Mark the new BLS key as valid in the contract state:
        // This typically involves hashing the new key and updating a mapping (blsKeys) to reflect that this key is now registered.
        blsKeys[_hashBlsKey(newBlsVK)] = true;

        // Emit an event to log the key update:
        // This event signals to off-chain listeners (e.g., user interfaces, monitoring services) that the validator has updated its consensus keys.
        emit ConsensusKeysUpdated(validator, newBlsVK, newSchnorrVK);
    }
}
