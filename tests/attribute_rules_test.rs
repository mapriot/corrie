use corrie::attribute_rules::{derive_attributes, MAIN_FLAG, SHOW_FLAG};

#[test]
fn elev_100_is_main_and_show_at_z11() {
    let (mz, fl) = derive_attributes(100);
    assert_eq!(mz, 11);
    assert!(fl & MAIN_FLAG != 0);
    assert!(fl & SHOW_FLAG != 0);
}

#[test]
fn elev_50_is_main_no_show_at_z11() {
    let (mz, fl) = derive_attributes(50);
    assert_eq!(mz, 11);
    assert!(fl & MAIN_FLAG != 0);
    assert!(fl & SHOW_FLAG == 0);
}

#[test]
fn elev_20_no_main_z12() {
    let (mz, fl) = derive_attributes(20);
    assert_eq!(mz, 12);
    assert_eq!(fl, 0);
}

#[test]
fn elev_10_z13() {
    let (mz, fl) = derive_attributes(10);
    assert_eq!(mz, 13);
    assert_eq!(fl, 0);
}

#[test]
fn elev_5_z14() {
    let (mz, fl) = derive_attributes(5);
    assert_eq!(mz, 14);
    assert_eq!(fl, 0);
}

#[test]
fn elev_15_z14() {
    let (mz, fl) = derive_attributes(15);
    assert_eq!(mz, 14);
    assert_eq!(fl, 0);
}

#[test]
fn elev_200_main_and_show() {
    let (mz, fl) = derive_attributes(200);
    assert_eq!(mz, 11);
    assert!(fl & MAIN_FLAG != 0);
    assert!(fl & SHOW_FLAG != 0);
}

#[test]
fn elev_negative_50_main() {
    // Below sea level – -50 is divisible by 50.
    let (mz, fl) = derive_attributes(-50);
    assert_eq!(mz, 11);
    assert!(fl & MAIN_FLAG != 0);
    assert!(fl & SHOW_FLAG == 0);
}

#[test]
fn elev_0_main_and_show() {
    // 0 is divisible by 100 (and 50 and 20 and 10).
    let (mz, fl) = derive_attributes(0);
    assert_eq!(mz, 11);
    assert!(fl & MAIN_FLAG != 0);
    assert!(fl & SHOW_FLAG != 0);
}
