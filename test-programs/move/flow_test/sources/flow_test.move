module flow_test::flow_test {
    #[test]
    fun test_computation() {
        let a: u64 = 10;
        let b: u64 = 32;
        let sum_val: u64 = a + b;
        assert!(sum_val == 42, 0);
        let doubled: u64 = sum_val * 2;
        assert!(doubled == 84, 1);
        let final_result: u64 = doubled + a;
        assert!(final_result == 94, 2);
    }
}
