//! AVM2 methods

use crate::avm2::activation::Activation;
use crate::avm2::class::Class;
use crate::avm2::error::{verify_error, Error};
use crate::avm2::script::TranslationUnit;
use crate::avm2::value::{abc_default_value, Value};
use crate::avm2::verify::{resolve_param_config, VerifiedMethodInfo};
use crate::avm2::Multiname;
use crate::tag_utils::SwfMovie;
use gc_arena::barrier::unlock;
use gc_arena::lock::RefLock;
use gc_arena::{Collect, Gc, GcCell, Mutation};
use std::borrow::Cow;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use swf::avm2::types::{
    AbcFile, Index, Method as AbcMethod, MethodBody as AbcMethodBody,
    MethodFlags as AbcMethodFlags, MethodParam as AbcMethodParam,
};

/// Represents a function defined in Ruffle's code.
///
/// Parameters are as follows:
///
///  * The AVM2 runtime
///  * The current `this` object
///  * The arguments this function was called with
///
/// Native functions are allowed to return a Value or an Error.
pub type NativeMethodImpl = for<'gc> fn(
    &mut Activation<'_, 'gc>,
    Value<'gc>,
    &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>>;

/// Configuration of a single parameter of a method,
/// with the parameter's type resolved.
#[derive(Clone, Collect, Debug)]
#[collect(no_drop)]
pub struct ResolvedParamConfig<'gc> {
    /// The type of the parameter.
    pub param_type: Option<Class<'gc>>,

    /// The default value for this parameter.
    pub default_value: Option<Value<'gc>>,
}

/// Configuration of a single parameter of a method.
#[derive(Clone, Collect, Debug)]
#[collect(no_drop)]
pub struct ParamConfig<'gc> {
    /// The name of the type of the parameter.
    pub param_type_name: Option<Gc<'gc, Multiname<'gc>>>,

    /// The default value for this parameter.
    pub default_value: Option<Value<'gc>>,
}

impl<'gc> ParamConfig<'gc> {
    fn from_abc_param(
        config: &AbcMethodParam,
        txunit: TranslationUnit<'gc>,
        activation: &mut Activation<'_, 'gc>,
    ) -> Result<Self, Error<'gc>> {
        let param_type_name = txunit.pool_multiname_static_any(activation, config.kind)?;

        let default_value = if let Some(dv) = &config.default_value {
            Some(abc_default_value(txunit, dv, activation)?)
        } else {
            None
        };

        Ok(Self {
            param_type_name,
            default_value,
        })
    }

    pub fn optional(
        param_type_name: Option<Gc<'gc, Multiname<'gc>>>,
        default_value: impl Into<Value<'gc>>,
    ) -> Self {
        Self {
            param_type_name,
            default_value: Some(default_value.into()),
        }
    }
}

/// Represents a reference to an AVM2 method and body.
#[derive(Collect)]
#[collect(no_drop)]
pub struct BytecodeMethod<'gc> {
    /// The translation unit this function was defined in.
    pub txunit: TranslationUnit<'gc>,

    /// The underlying ABC file of the above translation unit.
    #[collect(require_static)]
    abc: Rc<AbcFile>,

    /// The ABC method this function uses.
    pub abc_method: u32,

    /// The ABC method body this function uses.
    pub abc_method_body: Option<u32>,

    pub verified_info: RefLock<Option<VerifiedMethodInfo<'gc>>>,

    /// The parameter signature of this method.
    pub signature: Vec<ParamConfig<'gc>>,

    /// The return type of this method, or None if the method does not coerce
    /// its return value.
    pub return_type: Option<Gc<'gc, Multiname<'gc>>>,

    /// Whether or not this method was declared as a free-standing function.
    ///
    /// A free-standing function corresponds to the `Function` trait type, and
    /// is instantiated with the `newfunction` opcode.
    pub is_function: bool,

    /// Whether or not this method substitutes Undefined for missing arguments.
    ///
    /// This is true when the method is a free-standing function and none of the
    /// declared arguments have a type or a default value.
    pub is_unchecked: bool,
}

impl<'gc> BytecodeMethod<'gc> {
    /// Construct an `BytecodeMethod` from an `AbcFile` and method index.
    pub fn from_method_index(
        txunit: TranslationUnit<'gc>,
        abc_method: Index<AbcMethod>,
        is_function: bool,
        activation: &mut Activation<'_, 'gc>,
    ) -> Result<Self, Error<'gc>> {
        let abc = txunit.abc();
        let Some(method) = abc.methods.get(abc_method.0 as usize) else {
            return Err(Error::AvmError(verify_error(
                activation,
                "Error #1027: Method_info exceeds method_count.",
                1027,
            )?));
        };

        let mut signature = Vec::new();
        for param in &method.params {
            signature.push(ParamConfig::from_abc_param(param, txunit, activation)?);
        }

        let return_type = txunit.pool_multiname_static_any(activation, method.return_type)?;

        let abc_method_body = method.body.map(|b| b.0);

        let mut all_params_unchecked = true;
        for param in &signature {
            if param.param_type_name.is_some() || param.default_value.is_some() {
                all_params_unchecked = false;
            }
        }

        Ok(Self {
            txunit,
            abc: txunit.abc(),
            abc_method: abc_method.0,
            abc_method_body,
            verified_info: RefLock::new(None),
            signature,
            return_type,
            is_function,
            is_unchecked: is_function && all_params_unchecked,
        })
    }

    /// Get the underlying ABC file.
    pub fn abc(&self) -> Rc<AbcFile> {
        self.txunit.abc()
    }

    /// Get the underlying translation unit this method was defined in.
    pub fn translation_unit(&self) -> TranslationUnit<'gc> {
        self.txunit
    }

    /// Get a reference to the ABC method entry this refers to.
    pub fn method(&self) -> &AbcMethod {
        self.abc.methods.get(self.abc_method as usize).unwrap()
    }

    /// Get a reference to the SwfMovie this method came from.
    pub fn owner_movie(&self) -> Arc<SwfMovie> {
        self.txunit.movie()
    }

    /// Get a reference to the ABC method body entry this refers to.
    ///
    /// Some methods do not have bodies; this returns `None` in that case.
    pub fn body(&self) -> Option<&AbcMethodBody> {
        if let Some(abc_method_body) = self.abc_method_body {
            self.abc.method_bodies.get(abc_method_body as usize)
        } else {
            None
        }
    }

    #[inline(never)]
    pub fn verify(
        this: Gc<'gc, BytecodeMethod<'gc>>,
        activation: &mut Activation<'_, 'gc>,
    ) -> Result<(), Error<'gc>> {
        // TODO: avmplus seems to eaglerly verify some methods

        *unlock!(
            Gc::write(activation.gc(), this),
            BytecodeMethod,
            verified_info
        )
        .borrow_mut() = Some(crate::avm2::verify::verify_method(activation, this)?);

        Ok(())
    }

    /// Get the list of method params for this method.
    pub fn signature(&self) -> &[ParamConfig<'gc>] {
        &self.signature
    }

    pub fn resolved_return_type(&self) -> Option<Class<'gc>> {
        let verified_info = self.verified_info.borrow();

        verified_info.as_ref().unwrap().return_type
    }

    /// Get the name of this method.
    pub fn method_name(&self) -> Cow<'_, str> {
        let name_index = self.method().name.0 as usize;
        if name_index == 0 {
            return Cow::Borrowed("");
        }

        self.abc
            .constant_pool
            .strings
            .get(name_index - 1)
            .map(|s| String::from_utf8_lossy(s))
            .unwrap_or(Cow::Borrowed(""))
    }

    /// Determine if a given method is variadic.
    ///
    /// Variadic methods shove excess parameters into a final register.
    pub fn is_variadic(&self) -> bool {
        self.method()
            .flags
            .intersects(AbcMethodFlags::NEED_ARGUMENTS | AbcMethodFlags::NEED_REST)
    }

    /// Determine if a given method is unchecked.
    ///
    /// A method is unchecked if both of the following are true:
    ///
    ///  * The method was declared as a free-standing function
    ///  * The function's parameters have no declared types or default values
    pub fn is_unchecked(&self) -> bool {
        self.is_unchecked
    }
}

/// An uninstantiated method
#[derive(Clone, Collect)]
#[collect(no_drop)]
pub struct NativeMethod<'gc> {
    /// The function to call to execute the method.
    #[collect(require_static)]
    pub method: NativeMethodImpl,

    /// The name of the method.
    pub name: &'static str,

    /// The parameter signature of the method.
    pub signature: Vec<ParamConfig<'gc>>,

    /// The resolved parameter signature of the method.
    pub resolved_signature: GcCell<'gc, Option<Vec<ResolvedParamConfig<'gc>>>>,

    /// The return type of this method, or None if the method does not coerce
    /// its return value.
    pub return_type: Option<Gc<'gc, Multiname<'gc>>>,

    /// Whether or not this method accepts parameters beyond those
    /// mentioned in the parameter list.
    pub is_variadic: bool,
}

impl<'gc> NativeMethod<'gc> {
    pub fn resolve_signature(
        &self,
        activation: &mut Activation<'_, 'gc>,
    ) -> Result<(), Error<'gc>> {
        *self.resolved_signature.write(activation.gc()) =
            Some(resolve_param_config(activation, &self.signature)?);

        Ok(())
    }
}

impl fmt::Debug for NativeMethod<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeMethod")
            .field("method", &format!("{:p}", &self.method))
            .field("name", &self.name)
            .field("signature", &self.signature)
            .field("is_variadic", &self.is_variadic)
            .finish()
    }
}

/// An uninstantiated method that can either be natively implemented or sourced
/// from an ABC file.
#[derive(Copy, Clone, Collect)]
#[collect(no_drop)]
pub enum Method<'gc> {
    /// A native method.
    Native(Gc<'gc, NativeMethod<'gc>>),

    /// An ABC-provided method entry.
    Bytecode(Gc<'gc, BytecodeMethod<'gc>>),
}

impl<'gc> From<Gc<'gc, BytecodeMethod<'gc>>> for Method<'gc> {
    fn from(bm: Gc<'gc, BytecodeMethod<'gc>>) -> Self {
        Self::Bytecode(bm)
    }
}

impl<'gc> Method<'gc> {
    /// Define a builtin method with a particular param configuration.
    pub fn from_builtin_and_params(
        method: NativeMethodImpl,
        name: &'static str,
        signature: Vec<ParamConfig<'gc>>,
        return_type: Option<Gc<'gc, Multiname<'gc>>>,
        is_variadic: bool,
        mc: &Mutation<'gc>,
    ) -> Self {
        Self::Native(Gc::new(
            mc,
            NativeMethod {
                method,
                name,
                signature,
                resolved_signature: GcCell::new(mc, None),
                return_type,
                is_variadic,
            },
        ))
    }

    /// Define a builtin with no parameter constraints.
    pub fn from_builtin(method: NativeMethodImpl, name: &'static str, mc: &Mutation<'gc>) -> Self {
        Self::Native(Gc::new(
            mc,
            NativeMethod {
                method,
                name,
                signature: Vec::new(),
                resolved_signature: GcCell::new(mc, None),
                // FIXME - take in the real return type. This is needed for 'describeType'
                return_type: None,
                is_variadic: true,
            },
        ))
    }

    /// Access the bytecode of this method.
    ///
    /// This function returns `None` if this is a native method.
    pub fn into_bytecode(self) -> Option<Gc<'gc, BytecodeMethod<'gc>>> {
        match self {
            Method::Native { .. } => None,
            Method::Bytecode(bm) => Some(bm),
        }
    }

    pub fn return_type(&self) -> Option<Gc<'gc, Multiname<'gc>>> {
        match self {
            Method::Native(nm) => nm.return_type,
            Method::Bytecode(bm) => bm.return_type,
        }
    }

    pub fn signature(&self) -> &[ParamConfig<'gc>] {
        match self {
            Method::Native(nm) => &nm.signature,
            Method::Bytecode(bm) => bm.signature(),
        }
    }

    pub fn is_variadic(&self) -> bool {
        match self {
            Method::Native(nm) => nm.is_variadic,
            Method::Bytecode(bm) => bm.is_variadic(),
        }
    }

    /// Check if this method needs `arguments`.
    pub fn needs_arguments_object(&self) -> bool {
        match self {
            Method::Native { .. } => false,
            Method::Bytecode(bm) => bm.method().flags.contains(AbcMethodFlags::NEED_ARGUMENTS),
        }
    }
}
