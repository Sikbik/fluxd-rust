use fluxd_storage::Column;

#[test]
fn column_index_and_bit_are_stable() {
    let mut seen = 0u32;
    for (idx, column) in Column::ALL.iter().copied().enumerate() {
        assert_eq!(column.index(), idx);
        let bit = column.bit();
        assert_eq!(bit, 1u32 << idx);
        assert_eq!(bit.count_ones(), 1);
        assert_eq!(seen & bit, 0, "duplicate bit for {column:?}");
        seen |= bit;
    }
    assert_eq!(seen.count_ones() as usize, Column::ALL.len());
}
