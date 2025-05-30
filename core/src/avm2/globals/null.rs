use crate::avm2::activation::Activation;
use crate::avm2::class::{Class, ClassAttributes};
use crate::avm2::method::Method;
use crate::avm2::value::Value;
use crate::avm2::Error;
use crate::avm2::QName;
use ruffle_macros::istr;

fn null_init<'gc>(
    _activation: &mut Activation<'_, 'gc>,
    _this: Value<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    unreachable!()
}

pub fn create_class<'gc>(activation: &mut Activation<'_, 'gc>) -> Class<'gc> {
    let mc = activation.gc();
    let class = Class::custom_new(
        QName::new(activation.avm2().namespaces.public_all(), istr!("null")),
        None,
        Method::from_builtin(null_init, "", mc),
        vec![],
        mc,
    );
    class.set_attributes(mc, ClassAttributes::FINAL | ClassAttributes::SEALED);

    class
}
