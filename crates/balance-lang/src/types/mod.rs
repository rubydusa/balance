pub mod check;
pub mod infer;

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    Int,
    Float,
    Bool,
    None,
    Unit,
    Cap(String),
    Struct {
        name: String,
        fields: Vec<(String, Type)>,
    },
    Optional(Box<Type>),
    List(Box<Type>),
    Map(Box<Type>, Box<Type>),
    Ack,
    Observed(Box<Type>, Box<Type>),
    Interaction(Box<Type>),
    Named(String),
}
