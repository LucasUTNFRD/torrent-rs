pub struct Message {
    msg_type: MessageType,
}

pub enum MessageType {
    Error,
    Request(Request),
    Response(Response),
}

pub enum Request {
    Ping(PingRequest),
}

pub enum Response {
    Ping(PingResponse),
}

#[derive(Debug)]
struct PingRequest {}

#[derive(Debug)]
struct PingResponse {}
