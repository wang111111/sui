// Copyright (c) 2021, Facebook, Inc. and its affiliates
// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::authority::authority_tests::{
    call_move, call_move_, execute_programmable_transaction, init_state_with_ids,
    send_and_confirm_transaction, TestCallArg,
};
use move_core_types::identifier::Identifier;
use sui_types::{
    error::ExecutionErrorKind, object::Data,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    utils::to_sender_signed_transaction,
};

use move_core_types::language_storage::TypeTag;
use move_package::source_package::manifest_parser;
use sui_framework_build::compiled_package::{
    check_unpublished_dependencies, gather_dependencies, BuildConfig,
};
use sui_types::{
    crypto::{get_key_pair, AccountKeyPair},
    error::SuiError,
    messages::ExecutionStatus,
};

use expect_test::expect;
use std::fs::File;
use std::io::Read;
use std::{collections::HashSet, path::PathBuf};
use std::{env, str::FromStr};

const MAX_GAS: u64 = 10000;

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_publishing_with_unpublished_deps() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "depends_on_basics",
        /* with_unpublished_deps */ true,
    )
    .await;

    let ObjectRead::Exists(read_ref, package_obj, _) = authority
        .get_object_read(&package.0)
        .await
        .unwrap()
    else {
        panic!("Can't read package")
    };

    assert_eq!(package, read_ref);
    let Data::Package(move_package) = package_obj.data else {
        panic!("Not a package")
    };

    // Check that the published package includes its depended upon module.
    assert_eq!(
        move_package
            .serialized_module_map()
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>(),
        HashSet::from(["depends_on_basics", "object_basics"]),
    );

    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "depends_on_basics",
        "delegate",
        vec![],
        vec![],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    assert_eq!(effects.created().len(), 1);
    let ((_, v, _), owner) = effects.created()[0];

    // Check that calling the function does what we expect
    assert!(matches!(
        owner,
        Owner::Shared { initial_shared_version: initial } if initial == v
    ));
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_publish_empty_package() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;
    let gas_object = authority.get_object(&gas).await.unwrap();
    let gas_object_ref = gas_object.unwrap().compute_object_reference();

    // empty package
    let data =
        TransactionData::new_module_with_dummy_gas_price(sender, gas_object_ref, vec![], MAX_GAS);
    let transaction = to_sender_signed_transaction(data, &sender_key);
    let err = send_and_confirm_transaction(&authority, transaction)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SuiError::UserInputError {
            error: UserInputError::EmptyCommandInput
        }
    );

    // empty module
    let data = TransactionData::new_module_with_dummy_gas_price(
        sender,
        gas_object_ref,
        vec![vec![]],
        MAX_GAS,
    );
    let transaction = to_sender_signed_transaction(data, &sender_key);
    let result = send_and_confirm_transaction(&authority, transaction)
        .await
        .unwrap()
        .1;
    assert_eq!(
        result.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::VMVerificationOrDeserializationError,
            command: Some(0)
        }
    )
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_publish_duplicate_modules() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;
    let gas_object = authority.get_object(&gas).await.unwrap();
    let gas_object_ref = gas_object.unwrap().compute_object_reference();

    // empty package
    let mut modules = build_test_package("object_owner", /* with_unpublished_deps */ false);
    assert_eq!(modules.len(), 1);
    modules.push(modules[0].clone());
    let data =
        TransactionData::new_module_with_dummy_gas_price(sender, gas_object_ref, modules, MAX_GAS);
    let transaction = to_sender_signed_transaction(data, &sender_key);
    let result = send_and_confirm_transaction(&authority, transaction)
        .await
        .unwrap()
        .1;
    assert_eq!(
        result.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::VMVerificationOrDeserializationError,
            command: Some(0)
        }
    )
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_object_wrapping_unwrapping() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "object_wrapping",
        /* with_unpublished_deps */ false,
    )
    .await;

    let gas_version = authority.get_object(&gas).await.unwrap().unwrap().version();
    let create_child_version = SequenceNumber::lamport_increment([gas_version]);

    // Create a Child object.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_wrapping",
        "create_child",
        vec![],
        vec![],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let child_object_ref = effects.created()[0].0;
    assert_eq!(child_object_ref.1, create_child_version);

    let wrapped_version =
        SequenceNumber::lamport_increment([child_object_ref.1, effects.gas_object().0 .1]);

    // Create a Parent object, by wrapping the child object.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_wrapping",
        "create_parent",
        vec![],
        vec![TestCallArg::Object(child_object_ref.0)],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    // Child object is wrapped, Parent object is created.
    assert_eq!(
        (
            effects.created().len(),
            effects.deleted().len(),
            effects.unwrapped_then_deleted().len(),
            effects.wrapped().len()
        ),
        (1, 0, 0, 1)
    );
    let new_child_object_ref = effects.wrapped()[0];
    let expected_child_object_ref = (
        child_object_ref.0,
        wrapped_version,
        ObjectDigest::OBJECT_DIGEST_WRAPPED,
    );
    // Make sure that the child's version gets increased after wrapped.
    assert_eq!(new_child_object_ref, expected_child_object_ref);
    check_latest_object_ref(&authority, &expected_child_object_ref, true).await;

    let parent_object_ref = effects.created()[0].0;
    assert_eq!(parent_object_ref.1, wrapped_version);

    let unwrapped_version =
        SequenceNumber::lamport_increment([parent_object_ref.1, effects.gas_object().0 .1]);

    // Extract the child out of the parent.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_wrapping",
        "extract_child",
        vec![],
        vec![TestCallArg::Object(parent_object_ref.0)],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    // Check that the child shows up in unwrapped, not created.
    // mutated contains parent and gas.
    assert_eq!(
        (
            effects.mutated().len(),
            effects.created().len(),
            effects.unwrapped().len()
        ),
        (2, 0, 1)
    );
    // Make sure that version increments again when unwrapped.
    assert_eq!(effects.unwrapped()[0].0 .1, unwrapped_version);
    check_latest_object_ref(&authority, &effects.unwrapped()[0].0, false).await;
    let child_object_ref = effects.unwrapped()[0].0;

    let rewrap_version = SequenceNumber::lamport_increment([
        parent_object_ref.1,
        child_object_ref.1,
        effects.gas_object().0 .1,
    ]);

    // Wrap the child to the parent again.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_wrapping",
        "set_child",
        vec![],
        vec![
            TestCallArg::Object(parent_object_ref.0),
            TestCallArg::Object(child_object_ref.0),
        ],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    // Check that child object showed up in wrapped.
    // mutated contains parent and gas.
    assert_eq!((effects.mutated().len(), effects.wrapped().len()), (2, 1));
    let expected_child_object_ref = (
        child_object_ref.0,
        rewrap_version,
        ObjectDigest::OBJECT_DIGEST_WRAPPED,
    );
    assert_eq!(effects.wrapped()[0], expected_child_object_ref);
    check_latest_object_ref(&authority, &expected_child_object_ref, true).await;
    let child_object_ref = effects.wrapped()[0];
    let parent_object_ref = effects.mutated_excluding_gas().first().unwrap().0;

    let deleted_version =
        SequenceNumber::lamport_increment([parent_object_ref.1, effects.gas_object().0 .1]);

    // Now delete the parent object, which will in turn delete the child object.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_wrapping",
        "delete_parent",
        vec![],
        vec![TestCallArg::Object(parent_object_ref.0)],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    assert_eq!(effects.deleted().len(), 1);
    assert_eq!(effects.unwrapped_then_deleted().len(), 1);
    // Check that both objects are marked as deleted in the authority.
    let expected_child_object_ref = (
        child_object_ref.0,
        deleted_version,
        ObjectDigest::OBJECT_DIGEST_DELETED,
    );
    assert!(effects
        .unwrapped_then_deleted()
        .contains(&expected_child_object_ref));
    check_latest_object_ref(&authority, &expected_child_object_ref, true).await;
    let expected_parent_object_ref = (
        parent_object_ref.0,
        deleted_version,
        ObjectDigest::OBJECT_DIGEST_DELETED,
    );
    assert!(effects.deleted().contains(&expected_parent_object_ref));
    check_latest_object_ref(&authority, &expected_parent_object_ref, true).await;
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_object_owning_another_object() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "object_owner",
        /* with_unpublished_deps */ false,
    )
    .await;

    // Create a parent.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_parent",
        vec![],
        vec![],
    )
    .await
    .unwrap();
    assert!(effects.status().is_ok());
    let parent = effects.created()[0].0;

    // Create a child.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_child",
        vec![],
        vec![],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    let child = effects.created()[0].0;

    // Mutate the child directly should work fine.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "mutate_child",
        vec![],
        vec![TestCallArg::Object(child.0)],
    )
    .await
    .unwrap();
    assert!(effects.status().is_ok());

    // Add the child to the parent.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "add_child",
        vec![],
        vec![TestCallArg::Object(parent.0), TestCallArg::Object(child.0)],
    )
    .await
    .unwrap();
    effects.status().unwrap();
    let child_effect = effects
        .mutated()
        .iter()
        .find(|((id, _, _), _)| id == &child.0)
        .unwrap();
    // Check that the child is now owned by the parent.
    let field_id = match child_effect.1 {
        Owner::ObjectOwner(field_id) => field_id.into(),
        Owner::Shared { .. } | Owner::Immutable | Owner::AddressOwner(_) => panic!(),
    };
    let field_object = authority.get_object(&field_id).await.unwrap().unwrap();
    assert_eq!(field_object.owner, parent.0);

    // Mutate the child directly will now fail because we need the parent to authenticate.
    let result = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "mutate_child",
        vec![],
        vec![TestCallArg::Object(child.0)],
    )
    .await;
    assert!(result.is_err());

    // Mutate the child with the parent will not succeed.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "mutate_child_with_parent",
        vec![],
        vec![TestCallArg::Object(child.0), TestCallArg::Object(parent.0)],
    )
    .await;
    assert!(effects.is_err());
    assert!(format!("{effects:?}").contains("InvalidChildObjectArgument"));

    // Create another parent.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_parent",
        vec![],
        vec![],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    let new_parent = effects.created()[0].0;

    // Transfer the child to the new_parent.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "transfer_child",
        vec![],
        vec![
            TestCallArg::Object(parent.0),
            TestCallArg::Object(new_parent.0),
        ],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());

    // Delete the child. This should fail as the child cannot be used as a transaction argument
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "delete_child",
        vec![],
        vec![TestCallArg::Object(child.0)],
    )
    .await;
    assert!(effects.is_err());
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_create_then_delete_parent_child() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "object_owner",
        /* with_unpublished_deps */ false,
    )
    .await;

    // Create a parent and a child together
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_parent_and_child",
        vec![],
        vec![],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    // Creates 3 objects, the parent, a field, and the child
    assert_eq!(effects.created().len(), 3);
    let parent = effects
        .created()
        .iter()
        .find(|(_, owner)| matches!(owner, Owner::AddressOwner(_)))
        .unwrap()
        .0;

    // Delete the parent and child altogether.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "delete_parent_and_child",
        vec![],
        vec![TestCallArg::Object(parent.0)],
    )
    .await
    .unwrap();
    assert!(effects.status().is_ok());
    // Check that both objects were deleted.
    assert_eq!(effects.deleted().len(), 3);
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_create_then_delete_parent_child_wrap() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "object_owner",
        /* with_unpublished_deps */ false,
    )
    .await;

    // Create a parent and a child together
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_parent_and_child_wrapped",
        vec![],
        vec![],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    // Modifies the gas object
    assert_eq!(effects.mutated().len(), 1);
    // Creates 3 objects, the parent, a field, and the child
    assert_eq!(effects.created().len(), 2);
    // not wrapped as it wasn't first created
    assert_eq!(effects.wrapped().len(), 0);

    let gas_ref = effects.mutated()[0].0;

    let parent = effects
        .created()
        .iter()
        .find(|(_, owner)| matches!(owner, Owner::AddressOwner(_)))
        .unwrap()
        .0;

    let field = effects
        .created()
        .iter()
        .find(|((id, _, _), _)| id != &parent.0)
        .unwrap()
        .0;

    // Delete the parent and child altogether.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "delete_parent_and_child_wrapped",
        vec![],
        vec![TestCallArg::Object(parent.0)],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());

    // The parent and field are considered deleted, the child doesn't count because it wasn't
    // considered created in the first place.
    assert_eq!(effects.deleted().len(), 2);
    // The child was never created so it is not unwrapped.
    assert_eq!(effects.unwrapped_then_deleted().len(), 0);

    assert_eq!(
        effects
            .modified_at_versions()
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
        HashSet::from([
            (gas_ref.0, gas_ref.1),
            (parent.0, parent.1),
            (field.0, field.1)
        ]),
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_create_then_delete_parent_child_wrap_separate() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "object_owner",
        /* with_unpublished_deps */ false,
    )
    .await;

    // Create a parent.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_parent",
        vec![],
        vec![],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    let parent = effects.created()[0].0;

    // Create a child.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "create_child",
        vec![],
        vec![],
    )
    .await
    .unwrap();
    assert!(effects.status().is_ok());
    let child = effects.created()[0].0;

    // Add the child to the parent.
    println!("add_child_wrapped");
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "add_child_wrapped",
        vec![],
        vec![TestCallArg::Object(parent.0), TestCallArg::Object(child.0)],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    assert_eq!(effects.created().len(), 1);
    assert_eq!(effects.wrapped().len(), 1);

    // Delete the parent and child altogether.
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "object_owner",
        "delete_parent_and_child_wrapped",
        vec![],
        vec![TestCallArg::Object(parent.0)],
    )
    .await
    .unwrap();

    assert!(effects.status().is_ok());
    // Check that parent object was deleted.
    assert_eq!(effects.deleted().len(), 2);
    // Check that child object was unwrapped and deleted.
    assert_eq!(effects.unwrapped_then_deleted().len(), 1);
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_vector_empty() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_vector",
        /* with_unpublished_deps */ false,
    )
    .await;

    // call a function with an empty vector
    let type_tag =
        TypeTag::from_str(format!("{}::entry_point_vector::Obj", package.0).as_str()).unwrap();
    let pt = {
        let mut builder = ProgrammableTransactionBuilder::new();
        let empty_vec = builder.command(Command::MakeMoveVec(Some(type_tag.clone()), vec![]));
        builder.programmable_move_call(
            package.0,
            Identifier::new("entry_point_vector").unwrap(),
            Identifier::new("obj_vec_empty").unwrap(),
            vec![],
            vec![empty_vec],
        );
        builder.finish()
    };
    let effects = execute_programmable_transaction(&authority, &gas, &sender, &sender_key, pt)
        .await
        .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );

    // call a function with an empty vector whose type is generic
    let pt = {
        let mut builder = ProgrammableTransactionBuilder::new();
        let empty_vec = builder.command(Command::MakeMoveVec(Some(type_tag.clone()), vec![]));
        builder.programmable_move_call(
            package.0,
            Identifier::new("entry_point_vector").unwrap(),
            Identifier::new("type_param_vec_empty").unwrap(),
            vec![type_tag.clone()],
            vec![empty_vec],
        );
        builder.finish()
    };
    let effects = execute_programmable_transaction(&authority, &gas, &sender, &sender_key, pt)
        .await
        .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );

    // same tests again without the type tag
    // call a function with an empty vector, no type tag
    let pt = {
        let mut builder = ProgrammableTransactionBuilder::new();
        let empty_vec = builder.command(Command::MakeMoveVec(None, vec![]));
        builder.programmable_move_call(
            package.0,
            Identifier::new("entry_point_vector").unwrap(),
            Identifier::new("obj_vec_empty").unwrap(),
            vec![],
            vec![empty_vec],
        );
        builder.finish()
    };
    let err = execute_programmable_transaction(&authority, &gas, &sender, &sender_key, pt)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SuiError::UserInputError {
            error: UserInputError::EmptyCommandInput
        }
    );

    // call a function with an empty vector whose type is generic
    let pt = {
        let mut builder = ProgrammableTransactionBuilder::new();
        let empty_vec = builder.command(Command::MakeMoveVec(None, vec![]));
        builder.programmable_move_call(
            package.0,
            Identifier::new("entry_point_vector").unwrap(),
            Identifier::new("type_param_vec_empty").unwrap(),
            vec![type_tag],
            vec![empty_vec],
        );
        builder.finish()
    };
    let err = execute_programmable_transaction(&authority, &gas, &sender, &sender_key, pt)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SuiError::UserInputError {
            error: UserInputError::EmptyCommandInput
        }
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_vector_primitive() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_vector",
        /* with_unpublished_deps */ false,
    )
    .await;

    // just a test call with vector of 2 primitive values and check its length in the entry function
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "prim_vec_len",
        vec![],
        vec![TestCallArg::Pure(
            bcs::to_bytes(&vec![7_u64, 42_u64]).unwrap(),
        )],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_vector() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_vector",
        /* with_unpublished_deps */ false,
    )
    .await;

    // mint an owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "obj_vec_destroy",
        vec![],
        vec![TestCallArg::ObjVec(vec![obj_id])],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );

    // mint a parent object and a child object and make sure that parent stored in the vector
    // authenticates the child passed by-value
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (parent_id, _, _) = effects.created()[0].0;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_child",
        vec![],
        vec![
            TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap()),
            TestCallArg::Object(parent_id),
        ],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (child_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing the same owned object as another one passed as
    // a reference argument
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "child_access",
        vec![],
        vec![
            TestCallArg::Object(child_id),
            TestCallArg::ObjVec(vec![parent_id]),
        ],
    )
    .await;
    assert!(effects.is_err());
    assert!(format!("{effects:?}").contains("InvalidChildObjectArgument"));
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_vector_error() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_vector",
        /* with_unpublished_deps */ false,
    )
    .await;

    // mint an owned object of a wrong type
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_another",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "obj_vec_destroy",
        vec![],
        vec![TestCallArg::ObjVec(vec![obj_id])],
    )
    .await
    .unwrap();
    // should fail as we passed object of the wrong type
    assert!(
        matches!(effects.status(), ExecutionStatus::Failure { .. }),
        "{:?}",
        effects.status()
    );

    // mint two objects - one of a wrong type and one of the correct type
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_another",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (wrong_obj_id, _, _) = effects.created()[0].0;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (correct_obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "two_obj_vec_destroy",
        vec![],
        vec![TestCallArg::ObjVec(vec![wrong_obj_id, correct_obj_id])],
    )
    .await
    .unwrap();
    // should fail as we passed object of the wrong type as the first element of the vector
    assert!(
        matches!(effects.status(), ExecutionStatus::Failure { .. }),
        "{:?}",
        effects.status()
    );

    // mint a shared object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_shared",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (shared_obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one shared object
    let effects = call_move_(
        &authority,
        None,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "obj_vec_destroy",
        vec![],
        vec![TestCallArg::ObjVec(vec![shared_obj_id])],
        true, // shared object in arguments
    )
    .await
    .unwrap();
    // should fail as we do not support shared objects in vectors
    assert!(
        matches!(effects.status(), ExecutionStatus::Failure { .. }),
        "{:?}",
        effects.status()
    );

    // mint an owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing the same owned object as another one passed as
    // argument
    let result = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "same_objects",
        vec![],
        vec![
            TestCallArg::Object(obj_id),
            TestCallArg::ObjVec(vec![obj_id]),
        ],
    )
    .await;
    // should fail as we have the same object passed in vector and as a separate by-value argument
    assert_eq!(
        result.unwrap().status(),
        &ExecutionStatus::Failure {
            error: ExecutionErrorKind::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidUsageOfTakenValue,
            },
            command: Some(1)
        }
    );

    // mint an owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint",
        vec![],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing the same owned object as another one passed as
    // a reference argument
    let result = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "same_objects_ref",
        vec![],
        vec![
            TestCallArg::Object(obj_id),
            TestCallArg::ObjVec(vec![obj_id]),
        ],
    )
    .await;
    // should fail as we have the same object passed in vector and as a separate by-reference argument
    assert_eq!(
        result.unwrap().status(),
        &ExecutionStatus::Failure {
            error: ExecutionErrorKind::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidUsageOfTakenValue,
            },
            command: Some(1)
        }
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_vector_any() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_vector",
        /* with_unpublished_deps */ false,
    )
    .await;

    let any_type_tag =
        TypeTag::from_str(format!("{}::entry_point_vector::Any", package.0).as_str()).unwrap();

    // mint an owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "obj_vec_destroy_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::ObjVec(vec![obj_id])],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );

    // mint a parent object and a child object and make sure that parent stored in the vector
    // authenticates the child passed by-value
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (parent_id, _, _) = effects.created()[0].0;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_child_any",
        vec![any_type_tag.clone()],
        vec![
            TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap()),
            TestCallArg::Object(parent_id),
        ],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (child_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing the same owned object as another one passed as
    // a reference argument
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "child_access_any",
        vec![any_type_tag],
        vec![
            TestCallArg::Object(child_id),
            TestCallArg::ObjVec(vec![parent_id]),
        ],
    )
    .await;
    assert!(effects.is_err());
    assert!(format!("{effects:?}").contains("InvalidChildObjectArgument"));
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_vector_any_error() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_vector",
        /* with_unpublished_deps */ false,
    )
    .await;

    let any_type_tag =
        TypeTag::from_str(format!("{}::entry_point_vector::Any", package.0).as_str()).unwrap();

    // mint an owned object of a wrong type
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_another_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "obj_vec_destroy_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::ObjVec(vec![obj_id])],
    )
    .await
    .unwrap();
    // should fail as we passed object of the wrong type
    assert!(
        matches!(effects.status(), ExecutionStatus::Failure { .. }),
        "{:?}",
        effects.status()
    );

    // mint two objects - one of a wrong type and one of the correct type
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_another_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (wrong_obj_id, _, _) = effects.created()[0].0;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (correct_obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "two_obj_vec_destroy_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::ObjVec(vec![wrong_obj_id, correct_obj_id])],
    )
    .await
    .unwrap();
    // should fail as we passed object of the wrong type as the first element of the vector
    assert!(
        matches!(effects.status(), ExecutionStatus::Failure { .. }),
        "{:?}",
        effects.status()
    );

    // mint a shared object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_shared_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (shared_obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing one shared object
    let effects = call_move_(
        &authority,
        None,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "obj_vec_destroy_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::ObjVec(vec![shared_obj_id])],
        true, // shared object in arguments
    )
    .await
    .unwrap();
    // should fail as we do not support shared objects in vectors
    assert!(
        matches!(effects.status(), ExecutionStatus::Failure { .. }),
        "{:?}",
        effects.status()
    );

    // mint an owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing the same owned object as another one passed as
    // argument
    let result = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "same_objects_any",
        vec![any_type_tag.clone()],
        vec![
            TestCallArg::Object(obj_id),
            TestCallArg::ObjVec(vec![obj_id]),
        ],
    )
    .await;
    // should fail as we have the same object passed in vector and as a separate by-value argument
    assert_eq!(
        result.unwrap().status(),
        &ExecutionStatus::Failure {
            error: ExecutionErrorKind::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidUsageOfTakenValue,
            },
            command: Some(1)
        }
    );

    // mint an owned object
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "mint_any",
        vec![any_type_tag.clone()],
        vec![TestCallArg::Pure(bcs::to_bytes(&(42_u64)).unwrap())],
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );
    let (obj_id, _, _) = effects.created()[0].0;
    // call a function with a vector containing the same owned object as another one passed as
    // a reference argument
    let result = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_vector",
        "same_objects_ref_any",
        vec![any_type_tag.clone()],
        vec![
            TestCallArg::Object(obj_id),
            TestCallArg::ObjVec(vec![obj_id]),
        ],
    )
    .await;
    assert_eq!(
        result.unwrap().status(),
        &ExecutionStatus::Failure {
            error: ExecutionErrorKind::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidUsageOfTakenValue,
            },
            command: Some(1)
        }
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_string() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_string",
        /* with_unpublished_deps */ false,
    )
    .await;

    // pass a valid ascii string
    let ascii_str = "SomeString";
    let ascii_str_bcs = bcs::to_bytes(ascii_str).unwrap();
    let n = ascii_str.len() as u64;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "ascii_arg",
        vec![],
        vec![
            TestCallArg::Pure(ascii_str_bcs),
            TestCallArg::Pure(bcs::to_bytes(&n).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // pass a valid utf8 string
    let utf8_str = "çå∞≠¢õß∂ƒ∫";
    let utf_str_bcs = bcs::to_bytes(utf8_str).unwrap();
    let n = utf8_str.len() as u64;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "utf8_arg",
        vec![],
        vec![
            TestCallArg::Pure(utf_str_bcs),
            TestCallArg::Pure(bcs::to_bytes(&n).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // pass a valid longer utf8 string
    let utf8_str = "çå∞≠¢õß∂ƒ∫çå∞≠¢õß∂ƒ∫çå∞≠¢õß∂ƒ∫çå∞≠¢õß∂ƒ∫çå∞≠¢õß∂ƒ∫çå∞≠¢õß∂ƒ∫";
    let utf_str_bcs = bcs::to_bytes(utf8_str).unwrap();
    let n = utf8_str.len() as u64;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "utf8_arg",
        vec![],
        vec![
            TestCallArg::Pure(utf_str_bcs),
            TestCallArg::Pure(bcs::to_bytes(&n).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_nested_string() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_string",
        /* with_unpublished_deps */ false,
    )
    .await;

    // pass an option utf8 string
    let utf8_str = Some("çå∞≠¢õß∂ƒ∫");
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // vector option utf8 string
    let utf8_str = vec![Some("çå∞≠¢õß∂ƒ∫")];
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "vec_option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // vector option utf8 string
    let utf8_str = Some(vec![Some("çå∞≠¢õß∂ƒ∫")]);
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_vec_option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // pass an empty option utf8 string
    let utf8_str: Option<String> = None;
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // an empty vector option utf8 string
    let utf8_str: Vec<Option<String>> = vec![];
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "vec_option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // an vector of None
    let utf8_str: Vec<Option<String>> = vec![None, None];
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "vec_option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);

    // vector option utf8 string
    let utf8_str: Option<Vec<Option<String>>> = Some(vec![None, None]);
    let utf_str_bcs = bcs::to_bytes(&utf8_str).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_vec_option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_string_vec() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_string",
        /* with_unpublished_deps */ false,
    )
    .await;

    // pass a valid utf8 string vector
    let utf8_str_1 = "çå∞≠¢";
    let utf8_str_2 = "õß∂ƒ∫";
    let utf_str_vec_bcs = bcs::to_bytes(&vec![utf8_str_1, utf8_str_2]).unwrap();
    let total_len = (utf8_str_1.len() + utf8_str_2.len()) as u64;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "utf8_vec_arg",
        vec![],
        vec![
            TestCallArg::Pure(utf_str_vec_bcs),
            TestCallArg::Pure(bcs::to_bytes(&total_len).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(effects.status(), &ExecutionStatus::Success);
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_string_error() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_string",
        /* with_unpublished_deps */ false,
    )
    .await;

    // pass a utf string for ascii
    let utf8_str = "çå∞≠¢õß∂ƒ∫";
    let utf_str_bcs = bcs::to_bytes(utf8_str).unwrap();
    let n = utf8_str.len() as u64;
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "ascii_arg",
        vec![],
        vec![
            TestCallArg::Pure(utf_str_bcs),
            TestCallArg::Pure(bcs::to_bytes(&n).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );

    // pass a invalid ascii string
    let ascii_str = "SomeString";
    let n = ascii_str.len() as u64;
    let mut ascii_u8_vec = ascii_str.as_bytes().to_vec();
    // mess up one element
    ascii_u8_vec[7] = 255;

    let ascii_str_bcs = bcs::to_bytes(&ascii_u8_vec).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "ascii_arg",
        vec![],
        vec![
            TestCallArg::Pure(ascii_str_bcs),
            TestCallArg::Pure(bcs::to_bytes(&n).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );

    // pass a invalid utf8 string
    let utf8_str = "çå∞≠¢õß∂ƒ∫";
    let n = utf8_str.len() as u64;
    let mut utf8_u8_vec = utf8_str.as_bytes().to_vec();
    // mess up one element
    utf8_u8_vec[7] = 255;

    let utf8_str_bcs = bcs::to_bytes(&utf8_u8_vec).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "utf8_arg",
        vec![],
        vec![
            TestCallArg::Pure(utf8_str_bcs),
            TestCallArg::Pure(bcs::to_bytes(&n).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_string_vec_error() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_string",
        /* with_unpublished_deps */ false,
    )
    .await;

    // pass an utf8 string vector with one invalid string
    let utf8_str_1 = "çå∞≠¢";
    let utf8_str_2 = "õß∂ƒ∫";
    let total_len = (utf8_str_1.len() + utf8_str_2.len()) as u64;
    let mut utf8_u8_vec_1 = utf8_str_1.as_bytes().to_vec();
    // mess up one element
    utf8_u8_vec_1[7] = 255;
    let utf8_u8_vec_2 = utf8_str_2.as_bytes().to_vec();
    let utf_str_vec_bcs = bcs::to_bytes(&[utf8_u8_vec_1, utf8_u8_vec_2]).unwrap();

    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "utf8_vec_arg",
        vec![],
        vec![
            TestCallArg::Pure(utf_str_vec_bcs),
            TestCallArg::Pure(bcs::to_bytes(&total_len).unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_entry_point_string_option_error() {
    let (sender, sender_key): (_, AccountKeyPair) = get_key_pair();
    let gas = ObjectID::random();
    let authority = init_state_with_ids(vec![(sender, gas)]).await;

    let package = build_and_publish_test_package(
        &authority,
        &sender,
        &sender_key,
        &gas,
        "entry_point_string",
        /* with_unpublished_deps */ false,
    )
    .await;

    // pass an ascii string option with an invalid string
    let utf8_str = "çå∞≠¢õß∂ƒ∫";
    let utf_option_bcs = bcs::to_bytes(&Some(utf8_str)).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_ascii_arg",
        vec![],
        vec![TestCallArg::Pure(utf_option_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );

    // pass an utf8 string option with an invalid string
    let utf8_str = "çå∞≠¢";
    let mut utf8_u8_vec = utf8_str.as_bytes().to_vec();
    // mess up one element
    utf8_u8_vec[7] = 255;
    let utf_str_vec_bcs = bcs::to_bytes(&Some(utf8_u8_vec)).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_vec_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );

    // pass a vector as an option
    let utf8_str_1 = "çå∞≠¢";
    let utf8_str_2 = "õß∂ƒ∫";
    let utf_str_vec_bcs = bcs::to_bytes(&vec![utf8_str_1, utf8_str_2]).unwrap();
    let effects = call_move(
        &authority,
        &gas,
        &sender,
        &sender_key,
        &package.0,
        "entry_point_string",
        "option_utf8_arg",
        vec![],
        vec![TestCallArg::Pure(utf_str_vec_bcs)],
    )
    .await
    .unwrap();
    assert_eq!(
        effects.status(),
        &ExecutionStatus::Failure {
            error: ExecutionFailureStatus::CommandArgumentError {
                arg_idx: 0,
                kind: CommandArgumentError::InvalidBCSBytes
            },
            command: Some(0)
        }
    );
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_object_no_id_error() {
    let mut build_config = BuildConfig::new_for_testing();
    build_config.config.test_mode = true;
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // in this package object struct (NotObject) is defined incorrectly and publishing should
    // fail (it's defined in test-only code hence cannot be checked by transactional testing
    // framework which goes through "normal" publishing path which excludes tests).
    path.push("src/unit_tests/data/object_no_id/");
    let res = sui_framework::build_move_package(&path, build_config);

    matches!(res.err(), Some(SuiError::ExecutionError(err_str)) if
                 err_str.contains("SuiMoveVerificationError")
                 && err_str.contains("First field of struct NotObject must be 'id'"));
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_generate_lock_file() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.extend(["src", "unit_tests", "data", "generate_move_lock_file"]);

    let tmp = tempfile::tempdir().expect("Could not create temp dir for Move.lock");
    let lock_file_path = tmp.path().join("Move.lock");

    let mut build_config = BuildConfig::new_for_testing();
    build_config.config.lock_file = Some(lock_file_path.clone());
    sui_framework::build_move_package(&path, build_config).expect("Move package did not build");

    let mut lock_file_contents = String::new();
    File::open(lock_file_path)
        .expect("Cannot open lock file")
        .read_to_string(&mut lock_file_contents)
        .expect("Error reading Move.lock file");

    let expected = expect![[r##"
        # @generated by Move, please check-in and do not edit manually.

        [move]
        version = 0

        dependencies = [
          { name = "Examples" },
          { name = "Sui" },
        ]

        [[move.package]]
        name = "Examples"
        source = { local = "../object_basics" }

        dependencies = [
          { name = "Sui" },
        ]

        [[move.package]]
        name = "MoveStdlib"
        source = { local = "../../../../../sui-framework/deps/move-stdlib" }

        [[move.package]]
        name = "Sui"
        source = { local = "../../../../../sui-framework" }

        dependencies = [
          { name = "MoveStdlib" },
        ]
    "##]];
    expected.assert_eq(lock_file_contents.as_str());
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_custom_property_parse_published_at() {
    let build_config = BuildConfig::new_for_testing();
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.extend(["src", "unit_tests", "data", "custom_properties_in_manifest"]);

    sui_framework::build_move_package(&path, build_config).expect("Move package did not build");
    let manifest = manifest_parser::parse_move_manifest_from_file(path.as_path())
        .expect("Could not parse Move.toml");
    let properties = manifest
        .package
        .custom_properties
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<Vec<_>>();

    let expected = expect![[r#"
        [
            (
                "published-at",
                "0x777",
            ),
        ]
    "#]];
    expected.assert_debug_eq(&properties)
}

#[tokio::test]
#[cfg_attr(msim, ignore)]
async fn test_custom_property_check_unpublished_dependencies() {
    let build_config = BuildConfig::new_for_testing();
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.extend([
        "src",
        "unit_tests",
        "data",
        "custom_properties_in_manifest_ensure_published_at",
    ]);

    let resolution_graph = build_config
        .config
        .resolution_graph_for_package(&path, &mut std::io::sink())
        .expect("Could not build resolution graph.");

    let SuiError::ModulePublishFailure { error } =
        check_unpublished_dependencies(gather_dependencies(&resolution_graph).unpublished)
            .err()
            .unwrap()
     else {
        panic!("Expected ModulePublishFailure")
    };

    let expected = expect![[r#"
        Package dependency "CustomPropertiesInManifestDependencyMissingPublishedAt" does not specify a published address (the Move.toml manifest for "CustomPropertiesInManifestDependencyMissingPublishedAt" does not contain a published-at field).
        If this is intentional, you may use the --with-unpublished-dependencies flag to continue publishing these dependencies as part of your package (they won't be linked against existing packages on-chain)."#]];
    expected.assert_eq(&error)
}

pub fn build_test_package(test_dir: &str, with_unpublished_deps: bool) -> Vec<Vec<u8>> {
    let build_config = BuildConfig::new_for_testing();
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src");
    path.push("unit_tests");
    path.push("data");
    path.push(test_dir);
    sui_framework::build_move_package(&path, build_config)
        .unwrap()
        .get_package_bytes(with_unpublished_deps)
}

pub async fn build_and_try_publish_test_package(
    authority: &AuthorityState,
    sender: &SuiAddress,
    sender_key: &AccountKeyPair,
    gas_object_id: &ObjectID,
    test_dir: &str,
    gas_budget: u64,
    with_unpublished_deps: bool,
) -> (Transaction, SignedTransactionEffects) {
    let all_module_bytes = build_test_package(test_dir, with_unpublished_deps);

    let gas_object = authority.get_object(gas_object_id).await.unwrap();
    let gas_object_ref = gas_object.unwrap().compute_object_reference();

    let data = TransactionData::new_module_with_dummy_gas_price(
        *sender,
        gas_object_ref,
        all_module_bytes,
        gas_budget,
    );
    let transaction = to_sender_signed_transaction(data, sender_key);

    (
        transaction.clone().into_inner(),
        send_and_confirm_transaction(authority, transaction)
            .await
            .unwrap()
            .1,
    )
}

pub async fn build_and_publish_test_package(
    authority: &AuthorityState,
    sender: &SuiAddress,
    sender_key: &AccountKeyPair,
    gas_object_id: &ObjectID,
    test_dir: &str,
    with_unpublished_deps: bool,
) -> ObjectRef {
    build_and_publish_test_package_with_upgrade_cap(
        authority,
        sender,
        sender_key,
        gas_object_id,
        test_dir,
        with_unpublished_deps,
    )
    .await
    .0
}

pub async fn build_and_publish_test_package_with_upgrade_cap(
    authority: &AuthorityState,
    sender: &SuiAddress,
    sender_key: &AccountKeyPair,
    gas_object_id: &ObjectID,
    test_dir: &str,
    with_unpublished_deps: bool,
) -> (ObjectRef, ObjectRef) {
    let effects = build_and_try_publish_test_package(
        authority,
        sender,
        sender_key,
        gas_object_id,
        test_dir,
        MAX_GAS,
        with_unpublished_deps,
    )
    .await
    .1
    .into_data();
    assert!(
        matches!(effects.status(), ExecutionStatus::Success { .. }),
        "{:?}",
        effects.status()
    );

    let package = effects
        .created()
        .iter()
        .find(|(_, owner)| matches!(owner, Owner::Immutable))
        .unwrap();
    let upgrade_cap = effects
        .created()
        .iter()
        .find(|(_, owner)| matches!(owner, Owner::AddressOwner(_)))
        .unwrap();

    (package.0, upgrade_cap.0)
}

async fn check_latest_object_ref(
    authority: &AuthorityState,
    object_ref: &ObjectRef,
    expect_not_found: bool,
) {
    let response = authority
        .handle_object_info_request(ObjectInfoRequest {
            object_id: object_ref.0,
            object_format_options: None,
            request_kind: ObjectInfoRequestKind::LatestObjectInfo,
        })
        .await;
    if expect_not_found {
        assert!(matches!(
            UserInputError::try_from(response.unwrap_err()).unwrap(),
            UserInputError::ObjectNotFound { .. },
        ));
    } else {
        assert_eq!(
            &response.unwrap().object.compute_object_reference(),
            object_ref
        );
    }
}
