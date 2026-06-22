use types::OrderEvent;

// whenever a matcher creates a matched events it is pushed into event EventSink
// mathcer never fails. the concreate implementation behind it could be an ipc,
// or a Vec, it makes life easier for testing and abstracts future sink implementations
pub trait EventSink {
    fn emit(&mut self, event: OrderEvent);
}

impl EventSink for Vec<OrderEvent> {
    #[inline]
    fn emit(&mut self, event: OrderEvent) {
        self.push(event);
    }
}
