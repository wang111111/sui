// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

module serializer::serializer_tests {
    use sui::tx_context::{Self, TxContext};
    use sui::transfer;
    use sui::object::{Self, UID};

    struct MutableShared has key {
        id: UID,
        value: u64,
    }

     fun init(ctx: &mut TxContext) {
        transfer::share_object(MutableShared {
            id: object::new(ctx),
            value: 1,
        })
    }

    public entry fun list<T: key + store, C>(
        item: T,
        ctx: &mut TxContext
    ) {
        transfer::transfer(item, tx_context::sender(ctx))
    }

    public fun return_struct<T: key + store>(
        item: T,
    ): T {
        item
    }

    public entry fun value(clock: &MutableShared) {
        assert!(clock.value > 0, 1);
    }

    public entry fun set_value(clock: &mut MutableShared) {
        clock.value = 10;
    }

    public fun test_abort() {
        abort 0
    }
}