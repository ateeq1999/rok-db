//! `NUMERIC` (`Decimal`, feature `decimal`) and `INTERVAL` (`PgInterval`) columns.
#![cfg(all(feature = "testing", feature = "decimal"))]

use rok_db::prelude::*;
use rok_db::sqlx::postgres::types::PgInterval;
use rok_db::sqlx::types::Decimal;
use rok_db::{Projection, raw};

#[derive(Debug, Clone, PartialEq, Model)]
struct Plan {
    #[rok(primary_key, generated)]
    id: i64,
    name: String,
    price: Decimal,
    discount: Option<Decimal>,
    period: PgInterval,
    grace: Option<PgInterval>,
    tiers: Vec<Decimal>,
}

const PLANS: &str = "CREATE TABLE plans (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    price NUMERIC(10, 2) NOT NULL,
    discount NUMERIC(5, 4),
    period INTERVAL NOT NULL,
    grace INTERVAL,
    tiers NUMERIC[] NOT NULL
)";

fn money(cents: i64) -> Decimal {
    Decimal::new(cents, 2)
}

fn months(n: i32) -> PgInterval {
    PgInterval {
        months: n,
        days: 0,
        microseconds: 0,
    }
}

fn plan(name: &str, cents: i64, period_months: i32) -> Plan {
    Plan {
        id: 0,
        name: name.into(),
        price: money(cents),
        discount: None,
        period: months(period_months),
        grace: None,
        tiers: vec![money(100), money(250)],
    }
}

async fn seed(db: &Db) -> Vec<Plan> {
    db.execute(PLANS).await.unwrap();
    Plan::insert_all(
        db,
        &[
            plan("monthly", 999, 1),
            plan("yearly", 9_999, 12),
            plan("quarterly", 2_799, 3),
        ],
    )
    .await
    .unwrap()
}

fn names(plans: Vec<Plan>) -> Vec<String> {
    plans.into_iter().map(|p| p.name).collect()
}

#[rok_db::test]
async fn decimals_round_trip_filter_and_sort(db: Db) {
    let plans = seed(&db).await;
    assert_eq!(plans[0].price, money(999));
    assert_eq!(plans[0].tiers, [money(100), money(250)]);

    let cheap = Plan::filter(Plan::PRICE.lt(money(3_000)))
        .order_by(Plan::PRICE)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(names(cheap), ["monthly", "quarterly"]);

    // Scale is kept exactly: 0.1250, not a float approximation.
    let discount = Decimal::new(1_250, 4);
    Plan::filter(Plan::NAME.eq("yearly"))
        .update()
        .set(Plan::DISCOUNT, Some(discount))
        .exec(&db)
        .await
        .unwrap();
    let yearly = Plan::filter(Plan::DISCOUNT.is_not_null())
        .one(&db)
        .await
        .unwrap();
    assert_eq!(yearly.discount, Some(discount));
    assert_eq!(yearly.discount.unwrap().to_string(), "0.1250");
    let total: Decimal = raw("SELECT SUM(price) FROM plans")
        .scalar(&db)
        .await
        .unwrap();
    assert_eq!(total, money(999 + 9_999 + 2_799));
}

#[rok_db::test]
async fn intervals_round_trip_filter_and_sort(db: Db) {
    seed(&db).await;
    let short = Plan::filter(Plan::PERIOD.lte(months(3)))
        .order_by(Plan::PERIOD.desc())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(names(short), ["quarterly", "monthly"]);

    let grace = PgInterval {
        months: 0,
        days: 3,
        microseconds: 43_200_000_000, // 12 hours
    };
    Plan::filter(Plan::NAME.eq("monthly"))
        .update()
        .set(Plan::GRACE, Some(grace))
        .exec(&db)
        .await
        .unwrap();
    let monthly = Plan::filter(Plan::GRACE.is_not_null())
        .one(&db)
        .await
        .unwrap();
    assert_eq!(monthly.grace, Some(grace));
    assert_eq!(
        Plan::filter(Plan::GRACE.is_null())
            .count(&db)
            .await
            .unwrap(),
        2
    );
}

#[rok_db::test]
async fn keyset_pagination_over_decimal_and_interval(db: Db) {
    seed(&db).await;
    // Sorting by an expression makes the cursor decode the sort key by its
    // PostgreSQL type, which is where NUMERIC and INTERVAL need support.
    for order in [
        Projection::<Plan>::raw("plans.price + ?", [money(0)]).asc(),
        Projection::<Plan>::raw("plans.period + ?", [months(0)]).asc(),
    ] {
        let first = Plan::query()
            .order_by(order.clone())
            .cursor_paginate(&db, None, 2)
            .await
            .unwrap();
        let next = first.next.clone().expect("a second page");
        // The cursor survives a round trip through its string form.
        let next: rok_db::Cursor = next.to_string().parse().unwrap();
        let rest = Plan::query()
            .order_by(order)
            .cursor_paginate(&db, Some(&next), 2)
            .await
            .unwrap();
        let mut all = names(first.items);
        all.extend(names(rest.items));
        assert_eq!(all, ["monthly", "quarterly", "yearly"]);
        assert!(rest.next.is_none());
    }
}

#[rok_db::test]
async fn copy_in_encodes_both(db: Db) {
    db.execute(PLANS).await.unwrap();
    let rows: Vec<Plan> = (1..=50)
        .map(|i| plan(&format!("p{i}"), i * 100, i as i32))
        .collect();
    assert_eq!(Plan::copy_in(&db, &rows).await.unwrap(), 50);
    let last = Plan::query()
        .order_by(Plan::PRICE.desc())
        .first(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((last.price, last.period), (money(5_000), months(50)));
}

#[rok_db::test]
async fn nan_is_a_decode_error_not_a_wrong_value(db: Db) {
    let result: Result<Decimal, _> = raw("SELECT 'NaN'::numeric").scalar(&db).await;
    assert!(result.is_err());
}
