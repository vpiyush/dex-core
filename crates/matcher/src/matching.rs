
use orderbook::OrderBook;
use types::{OrderEvent, OrderId, OrderRequest};

pub fn match_limit(book: &OrderBook, req: &OrderRequest, order_id: u64, out: &mut Vec<OrderEvent>) {
    todo!("limit matching: cross opposide side, emit fills and rest residual")
}

pub fn match_market(book: &OrderBook, req: &OrderRequest, order_id: u64, out: &mut Vec<OrderEvent>) {
    todo!("market matching: cross opposide side until exhausted or empty")

}
