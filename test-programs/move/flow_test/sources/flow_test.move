/// Comprehensive Move test module exercising a wide range of language constructs.
///
/// Each `#[test]` function targets a specific set of Move features and contains
/// clear breakpoint targets where local variables hold verifiable values.
/// The functions are designed to be recorded by `codetracer-move-recorder` and
/// inspected through the DAP flow interface.
module flow_test::flow_test {
    use std::vector;

    // -----------------------------------------------------------------------
    // Constants
    // -----------------------------------------------------------------------

    const E_INVALID_VALUE: u64 = 1;
    const E_EMPTY_VECTOR: u64 = 2;
    const E_OVERFLOW: u64 = 3;
    const SCALE_FACTOR: u64 = 100;

    // -----------------------------------------------------------------------
    // Structs
    // -----------------------------------------------------------------------

    /// A simple 2-D point with `copy` and `drop` abilities.
    struct Point has copy, drop {
        x: u64,
        y: u64,
    }

    /// A rectangle defined by its origin and dimensions.
    struct Rectangle has copy, drop {
        origin: Point,
        width: u64,
        height: u64,
    }

    /// A wrapper demonstrating `store` ability alongside `copy` and `drop`.
    struct Wallet has copy, drop, store {
        balance: u64,
        id: u64,
    }

    /// A generic container holding a single value.
    struct Container<T: copy + drop> has copy, drop {
        value: T,
        label: u64,
    }

    // -----------------------------------------------------------------------
    // Private helper functions
    // -----------------------------------------------------------------------

    /// Compute the area of a rectangle.
    fun rectangle_area(r: &Rectangle): u64 {
        r.width * r.height
    }

    /// Add two points component-wise.
    fun add_points(a: &Point, b: &Point): Point {
        Point { x: a.x + b.x, y: a.y + b.y }
    }

    /// Scale a point's coordinates by a given factor (mutates in place).
    fun scale_point(p: &mut Point, factor: u64) {
        p.x = p.x * factor;
        p.y = p.y * factor;
    }

    /// Compute n-th Fibonacci number iteratively.
    fun fibonacci(n: u64): u64 {
        if (n == 0) {
            return 0
        };
        if (n == 1) {
            return 1
        };
        let a: u64 = 0;
        let b: u64 = 1;
        let i: u64 = 2;
        while (i <= n) {
            let temp: u64 = b;
            b = a + b;
            a = temp;
            i = i + 1;
        };
        b
    }

    /// Return the maximum of two u64 values.
    fun max_u64(a: u64, b: u64): u64 {
        if (a >= b) { a } else { b }
    }

    /// Return the minimum of two u64 values.
    fun min_u64(a: u64, b: u64): u64 {
        if (a <= b) { a } else { b }
    }

    /// Compute the sum of a vector of u64 values.
    fun vector_sum(v: &vector<u64>): u64 {
        let sum: u64 = 0;
        let i: u64 = 0;
        let len: u64 = vector::length(v);
        while (i < len) {
            sum = sum + *vector::borrow(v, i);
            i = i + 1;
        };
        sum
    }

    /// Return multiple values as a tuple: (sum, product, max).
    fun compute_triple(a: u64, b: u64): (u64, u64, u64) {
        let sum = a + b;
        let product = a * b;
        let max = max_u64(a, b);
        (sum, product, max)
    }

    /// Wrap a value in a Container with a label.
    fun wrap_value<T: copy + drop>(val: T, label: u64): Container<T> {
        Container { value: val, label }
    }

    /// Unwrap a Container, returning its value.
    fun unwrap_value<T: copy + drop>(c: Container<T>): T {
        let Container { value, label: _ } = c;
        value
    }

    // -----------------------------------------------------------------------
    // Test: basic arithmetic across multiple integer widths
    // -----------------------------------------------------------------------

    #[test]
    fun test_computation() {
        let a: u64 = 10;
        let b: u64 = 32;
        let sum_val: u64 = a + b;
        assert!(sum_val == 42, E_INVALID_VALUE);
        let doubled: u64 = sum_val * 2;
        assert!(doubled == 84, E_INVALID_VALUE);
        let final_result: u64 = doubled + a;
        // breakpoint target: all five locals are in scope
        assert!(final_result == 94, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: struct creation, field access, and destructuring
    // -----------------------------------------------------------------------

    #[test]
    fun test_structs() {
        let p1 = Point { x: 3, y: 4 };
        let p2 = Point { x: 7, y: 6 };
        let sum_point = add_points(&p1, &p2);
        // breakpoint target: sum_point.x == 10, sum_point.y == 10
        assert!(sum_point.x == 10, E_INVALID_VALUE);
        assert!(sum_point.y == 10, E_INVALID_VALUE);

        let rect = Rectangle { origin: p1, width: 5, height: 8 };
        let area = rectangle_area(&rect);
        // breakpoint target: area == 40
        assert!(area == 40, E_INVALID_VALUE);

        // Destructure the point
        let Point { x: px, y: py } = sum_point;
        let sum_coords = px + py;
        // breakpoint target: px == 10, py == 10, sum_coords == 20
        assert!(sum_coords == 20, E_INVALID_VALUE);

        // Wallet with store ability
        let w = Wallet { balance: 1000, id: 1 };
        let wallet_balance = w.balance;
        // breakpoint target: wallet_balance == 1000
        assert!(wallet_balance == 1000, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: vector operations
    // -----------------------------------------------------------------------

    #[test]
    fun test_vectors() {
        let v = vector::empty<u64>();
        vector::push_back(&mut v, 10);
        vector::push_back(&mut v, 20);
        vector::push_back(&mut v, 30);
        vector::push_back(&mut v, 40);
        vector::push_back(&mut v, 50);

        let len = vector::length(&v);
        // breakpoint target: len == 5
        assert!(len == 5, E_EMPTY_VECTOR);

        let first = *vector::borrow(&v, 0);
        let last = *vector::borrow(&v, 4);
        // breakpoint target: first == 10, last == 50
        assert!(first == 10, E_INVALID_VALUE);
        assert!(last == 50, E_INVALID_VALUE);

        let sum = vector_sum(&v);
        // breakpoint target: sum == 150
        assert!(sum == 150, E_INVALID_VALUE);

        // Pop and verify
        let popped = vector::pop_back(&mut v);
        let new_len = vector::length(&v);
        // breakpoint target: popped == 50, new_len == 4
        assert!(popped == 50, E_INVALID_VALUE);
        assert!(new_len == 4, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: loops (while, loop/break) and control flow
    // -----------------------------------------------------------------------

    #[test]
    fun test_loops() {
        // While loop: sum 1..10
        let counter: u64 = 0;
        let accumulator: u64 = 0;
        while (counter < 10) {
            counter = counter + 1;
            accumulator = accumulator + counter;
        };
        // breakpoint target: counter == 10, accumulator == 55
        assert!(counter == 10, E_INVALID_VALUE);
        assert!(accumulator == 55, E_INVALID_VALUE);

        // loop with break: find first power of 2 >= 100
        let power: u64 = 1;
        let iterations: u64 = 0;
        loop {
            if (power >= 100) {
                break
            };
            power = power * 2;
            iterations = iterations + 1;
        };
        // breakpoint target: power == 128, iterations == 7
        assert!(power == 128, E_INVALID_VALUE);
        assert!(iterations == 7, E_INVALID_VALUE);

        // Conditional branching
        let grade = if (accumulator > 50) { 1u64 } else { 0u64 };
        // breakpoint target: grade == 1
        assert!(grade == 1, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: nested function calls and tuple returns
    // -----------------------------------------------------------------------

    #[test]
    fun test_nested_calls() {
        let x: u64 = 12;
        let y: u64 = 8;

        let (sum, product, max) = compute_triple(x, y);
        // breakpoint target: sum == 20, product == 96, max == 12
        assert!(sum == 20, E_INVALID_VALUE);
        assert!(product == 96, E_INVALID_VALUE);
        assert!(max == 12, E_INVALID_VALUE);

        // Nested calls: max of min values
        let nested_result = max_u64(min_u64(x, y), min_u64(15, 20));
        // breakpoint target: nested_result == 15
        assert!(nested_result == 15, E_INVALID_VALUE);

        // Chain operations with constants
        let scaled = product * SCALE_FACTOR;
        // breakpoint target: scaled == 9600
        assert!(scaled == 9600, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: generic functions
    // -----------------------------------------------------------------------

    #[test]
    fun test_generics() {
        // Wrap a u64
        let c1 = wrap_value<u64>(42, 1);
        let v1 = unwrap_value<u64>(c1);
        // breakpoint target: v1 == 42
        assert!(v1 == 42, E_INVALID_VALUE);

        // Wrap a bool
        let c2 = wrap_value<bool>(true, 2);
        let v2 = unwrap_value<bool>(c2);
        assert!(v2 == true, E_INVALID_VALUE);

        // Wrap a Point
        let pt = Point { x: 5, y: 10 };
        let c3 = wrap_value<Point>(pt, 3);
        let v3 = unwrap_value<Point>(c3);
        // breakpoint target: v3.x == 5, v3.y == 10, label == 3
        assert!(v3.x == 5, E_INVALID_VALUE);
        assert!(v3.y == 10, E_INVALID_VALUE);
        let container_label = c3.label;
        // breakpoint target: container_label == 3
        assert!(container_label == 3, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: Fibonacci computation
    // -----------------------------------------------------------------------

    #[test]
    fun test_fibonacci() {
        let fib_0 = fibonacci(0);
        let fib_1 = fibonacci(1);
        let fib_5 = fibonacci(5);
        let fib_10 = fibonacci(10);
        let fib_15 = fibonacci(15);

        // breakpoint target: fib_0 == 0, fib_1 == 1, fib_5 == 5, fib_10 == 55, fib_15 == 610
        assert!(fib_0 == 0, E_INVALID_VALUE);
        assert!(fib_1 == 1, E_INVALID_VALUE);
        assert!(fib_5 == 5, E_INVALID_VALUE);
        assert!(fib_10 == 55, E_INVALID_VALUE);
        assert!(fib_15 == 610, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: mutable references
    // -----------------------------------------------------------------------

    #[test]
    fun test_references() {
        let mut_point = Point { x: 2, y: 3 };

        // Mutate via reference
        scale_point(&mut mut_point, 5);
        // breakpoint target: mut_point.x == 10, mut_point.y == 15
        assert!(mut_point.x == 10, E_INVALID_VALUE);
        assert!(mut_point.y == 15, E_INVALID_VALUE);

        // Immutable reference read
        let ref_x = &mut_point;
        let read_x = ref_x.x;
        let read_y = ref_x.y;
        // breakpoint target: read_x == 10, read_y == 15
        assert!(read_x == 10, E_INVALID_VALUE);
        assert!(read_y == 15, E_INVALID_VALUE);

        // Scale again
        scale_point(&mut mut_point, 3);
        let final_x = mut_point.x;
        let final_y = mut_point.y;
        // breakpoint target: final_x == 30, final_y == 45
        assert!(final_x == 30, E_INVALID_VALUE);
        assert!(final_y == 45, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: boolean logic and wider integer types
    // -----------------------------------------------------------------------

    #[test]
    fun test_boolean_and_integers() {
        // Boolean logic
        let t = true;
        let f = false;
        let and_result = t && f;
        let or_result = t || f;
        let not_result = !t;
        assert!(and_result == false, E_INVALID_VALUE);
        assert!(or_result == true, E_INVALID_VALUE);
        assert!(not_result == false, E_INVALID_VALUE);

        // u8 arithmetic
        let small_a: u8 = 200;
        let small_b: u8 = 55;
        let small_sum: u8 = small_a + small_b;
        // breakpoint target: small_sum == 255
        assert!(small_sum == 255, E_INVALID_VALUE);

        // u128 arithmetic
        let big_a: u128 = 1_000_000_000_000;
        let big_b: u128 = 2_000_000_000_000;
        let big_sum: u128 = big_a + big_b;
        // breakpoint target: big_sum == 3_000_000_000_000
        assert!(big_sum == 3_000_000_000_000, E_INVALID_VALUE);

        // Conditional with boolean
        let status: u64 = if (or_result && !and_result) { 1 } else { 0 };
        // breakpoint target: status == 1
        assert!(status == 1, E_INVALID_VALUE);
    }

    // -----------------------------------------------------------------------
    // Test: abort and error handling patterns
    // -----------------------------------------------------------------------

    #[test]
    #[expected_failure(abort_code = 42)]
    fun test_abort() {
        let x: u64 = 10;
        let y: u64 = 0;
        // This will abort
        if (y == 0) {
            abort 42
        };
        // Unreachable
        let _z = x / y;
    }
}
