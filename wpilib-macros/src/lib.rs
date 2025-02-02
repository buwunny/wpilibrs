use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use proc_macro2::TokenTree as TokenTree2;
use quote::{quote, ToTokens};
use std::collections::VecDeque;
use std::sync::atomic::AtomicU8;
use syn::visit_mut::VisitMut;
fn is_non_static_method(method: &syn::ImplItemFn) -> bool {
    if !method.sig.inputs.is_empty() {
        matches!(method.sig.inputs.first().unwrap(), syn::FnArg::Receiver(_))
    } else {
        false
    }
}
#[allow(dead_code)]
fn is_static_method(method: &syn::ImplItemFn) -> bool {
    !is_non_static_method(method)
}
fn method_takes_only_self_ref(method: &syn::ImplItemFn) -> bool {
    let name = method.sig.ident.to_string();
    method.sig.inputs.len() == 1
        && match method
            .sig
            .inputs
            .first()
            .unwrap_or_else(|| panic!("expected a receiver as first arg on {}", name))
        {
            syn::FnArg::Receiver(receiver) => {
                receiver.reference.is_some() && receiver.mutability.is_none()
            }
            _ => false,
        }
}
fn is_public_method(method: &syn::ImplItemFn) -> bool {
    matches!(method.vis, syn::Visibility::Public(_))
}
fn method_return_type_is(method: &syn::ImplItemFn, ty: &str) -> bool {
    match method.sig.output {
        syn::ReturnType::Default => ty.is_empty(),
        syn::ReturnType::Type(_, ref t) => match **t {
            syn::Type::Path(ref path) => path.path.segments.last().unwrap().ident == ty,
            _ => false,
        },
    }
}

#[proc_macro_attribute]
pub fn subsystem_methods(_attr: TokenStream, input: TokenStream) -> TokenStream {
    // throw error if input is not an impl
    let implementation = syn::parse_macro_input!(input as syn::ItemImpl);

    // get the name of the struct being implemented
    let struct_name = match *implementation.self_ty {
        syn::Type::Path(ref path) => path.path.segments.last().unwrap().ident.clone(),
        _ => panic!("expected a struct"),
    };

    //get the struct name in caps as an identifier
    let struct_name_caps = syn::Ident::new(
        &format!("__{}", struct_name.to_string().to_uppercase()),
        struct_name.span(),
    );

    let mut impl_block = Vec::new();

    // go through all funcs, if none are decorated with `#[new]` then throw an error
    let mut new_func = None;
    let mut periodic_func = None;
    let mut test_command_func = None;
    let mut default_command_func = None;
    let mut other_funcs = Vec::new();
    for item in implementation.items {
        if let syn::ImplItem::Fn(method) = item {
            let mut attrs = method.attrs.iter().clone();

            //for ignor attributes just skip the function
            if attrs
                .clone()
                .any(|attr| attr.path().is_ident("dont_static"))
            {
                let mut new_method = method.clone();
                new_method.attrs = Vec::new();
                impl_block.push(new_method);
                continue;
            }

            if attrs.clone().any(|attr| attr.path().is_ident("new")) {
                if new_func.is_some() {
                    panic!("expected only one function decorated with `#[new]`");
                }
                if is_non_static_method(&method) {
                    panic!("expected function decorated with `#[new]` to be static");
                }
                new_func = Some(method);
                continue;
            }
            if attrs.clone().any(|attr| attr.path().is_ident("periodic")) {
                if periodic_func.is_some() {
                    panic!("expected only one function decorated with `#[periodic]`");
                }
                if !method_takes_only_self_ref(&method) {
                    panic!("expected function decorated with `#[periodic]` to take only a self reference");
                }
                if !method_return_type_is(&method, "") {
                    panic!("expected function decorated with `#[periodic]` to return nothing");
                }
                periodic_func = Some(method);
                continue;
            }
            if attrs
                .clone()
                .any(|attr| attr.path().is_ident("test_command"))
            {
                if test_command_func.is_some() {
                    panic!("expected only one function decorated with `#[test_command]`");
                }
                if !method_return_type_is(&method, "Command") {
                    panic!(
                        "expected function decorated with `#[test_command]` to return a Command"
                    );
                }
                if !method_takes_only_self_ref(&method) {
                    panic!("expected function decorated with `#[test_command]` to take only a self reference");
                }
                test_command_func = Some(method);
                continue;
            }
            if attrs.any(|attr| attr.path().is_ident("default_command")) {
                let new_method = method.clone();
                if default_command_func.is_some() {
                    panic!("expected only one function decorated with `#[default_command]`");
                }
                if !method_return_type_is(&method, "Command") {
                    panic!(
                        "expected function decorated with `#[default_command]` to return a Command"
                    );
                }
                if !method_takes_only_self_ref(&method) {
                    panic!("expected function decorated with `#[default_command]` to take only a self reference");
                }
                default_command_func = Some(new_method);
            }

            let requires_self = is_non_static_method(&method);
            let is_public = is_public_method(&method);
            if requires_self && is_public {
                other_funcs.push(method);
            } else {
                impl_block.push(method);
            }
        }
    }
    if new_func.is_none() {
        panic!("expected a function decorated with `#[new]`");
    };

    // get the new function and rewrite it as private with name `__new`
    let mut new_func = new_func.unwrap();
    new_func.sig.ident = syn::Ident::new("__new", new_func.sig.ident.span());
    new_func.vis = syn::Visibility::Inherited;
    new_func.attrs = Vec::new();
    impl_block.push(new_func);

    //get the periodic function and rewrite it as a static function with name `periodic`
    if let Some(periodic_func) = periodic_func {
        let mut periodic_func = periodic_func;
        periodic_func.sig.ident = syn::Ident::new("periodic_inner", periodic_func.sig.ident.span());
        periodic_func.vis = syn::Visibility::Inherited;
        other_funcs.push(periodic_func);
        let static_func = syn::parse_quote! {
            pub fn periodic() {
                let mut this = #struct_name_caps.lock();
                this.__periodic_inner();
            }
        };
        impl_block.push(static_func);
    } else {
        let periodic_func = syn::parse_quote! {
            pub fn periodic() {}
        };
        impl_block.push(periodic_func);
    }

    if let Some(default_command_func) = default_command_func {
        //add __ infront of the ident
        let ident = syn::Ident::new(
            &format!("__{}", default_command_func.sig.ident),
            default_command_func.sig.ident.span(),
        );
        let static_func = syn::parse_quote! {
            pub fn default_command() -> Command {
                let mut this = #struct_name_caps.lock();
                this.#ident()
            }
        };
        impl_block.push(static_func);
    } else {
        let default_command_func = syn::parse_quote! {
            pub fn default_command() -> Command {
                Default::default()
            }
        };
        impl_block.push(default_command_func);
    }

    let fn_idents: Vec<String> = other_funcs
        .iter()
        .map(|func| func.sig.ident.to_string())
        .collect();

    //for each func in the impl block, make the non static version private and make a public static version
    for item_fn in &mut other_funcs {
        item_fn.attrs = Vec::new();

        //if item has lifetime or a lifetime in any of its args throw an error
        if item_fn.sig.generics.lifetimes().count() > 0
            || item_fn.sig.inputs.iter().any(|arg| match arg {
                syn::FnArg::Receiver(rec) => rec
                    .reference
                    .as_ref()
                    .map_or(false, |(_, lifetime)| lifetime.is_some()),
                syn::FnArg::Typed(arg) => match *arg.ty.clone() {
                    syn::Type::Reference(ref_type) => ref_type.lifetime.is_some(),
                    _ => false,
                },
            })
        {
            let ident_str = item_fn.sig.ident.to_string();
            panic!(
                "expected function `{}` to not have any lifetimes",
                ident_str
            );
        }

        let static_ident =
            syn::Ident::new(&format!("{}", item_fn.sig.ident), item_fn.sig.ident.span());

        //make the non static version private and rename it to __<name>
        item_fn.vis = syn::Visibility::Inherited;
        item_fn.sig.ident = syn::Ident::new(
            &format!("__{}", item_fn.sig.ident),
            item_fn.sig.ident.span(),
        );

        //scrape through the block and replace all instances any funcs in fn_idents with their __<name> version
        let block = item_fn.block.clone();
        //turn block into a token stream
        let block_stream = quote!(#block);
        //check all the idents of the block
        let mut new_stream = TokenStream2::new();
        fn clean_block(
            new_stream: &mut TokenStream2,
            old_stream: &TokenStream2,
            fn_idents: &Vec<String>,
        ) {
            let mut last_two_tokens: VecDeque<TokenTree2> = VecDeque::new();

            let check_last_two_tokens = |ltt: &VecDeque<TokenTree2>| {
                if ltt.len() > 1 {
                    ltt[0].to_string() == "self" && ltt[1].to_string() == "."
                } else {
                    false
                }
            };

            for token in old_stream.clone().into_iter() {
                if last_two_tokens.len() > 2 {
                    last_two_tokens.pop_front();
                }

                match token.clone() {
                    TokenTree2::Group(group) => {
                        let mut new_group_stream = TokenStream2::new();
                        clean_block(&mut new_group_stream, &group.stream(), fn_idents);
                        let new_group =
                            proc_macro2::Group::new(group.delimiter(), new_group_stream);
                        new_stream
                            .extend(std::iter::once(proc_macro2::TokenTree::Group(new_group)));
                        last_two_tokens.clear();
                        continue;
                    }
                    TokenTree2::Ident(ident) => {
                        if fn_idents.contains(&ident.to_string())
                            && check_last_two_tokens(&last_two_tokens)
                        {
                            //replace the ident with __<name>
                            let new_ident = syn::Ident::new(&format!("__{}", ident), ident.span());
                            new_stream
                                .extend(std::iter::once(proc_macro2::TokenTree::Ident(new_ident)));
                            last_two_tokens.clear();
                            continue;
                        }
                    }
                    _ => {}
                }
                last_two_tokens.push_back(token.clone());
                new_stream.extend(std::iter::once(token));
            }
        }
        clean_block(&mut new_stream, &block_stream, &fn_idents);
        //turn new stream back into block
        let new_block = syn::parse2::<syn::Block>(new_stream).expect("couldnt scrape block");
        //replace the block in the function with the new block
        item_fn.block = new_block;

        impl_block.push(item_fn.clone());

        let fn_str = item_fn.sig.ident.to_string();
        if fn_str == "default_command_inner" || fn_str == "periodic_inner" {
            continue;
        }

        // get all input idents
        let mut input_idents = Vec::new();
        let mut input_types = Vec::new();
        for arg in &item_fn.sig.inputs {
            if let syn::FnArg::Typed(pat_type) = arg {
                if let syn::Pat::Ident(pat_ident) = &*pat_type.pat {
                    input_idents.push(pat_ident.ident.clone());
                }
                input_types.push(pat_type.ty.clone());
            }
        }
        let non_static_ident = item_fn.sig.ident.clone();
        let non_static_output = item_fn.sig.output.clone();

        let static_fn_item = syn::parse_quote! {
            pub fn #static_ident(#(#input_idents: #input_types),*) #non_static_output {
                let mut this = #struct_name_caps.lock();
                this.#non_static_ident(#(#input_idents),*)
            }
        };

        impl_block.push(static_fn_item);
    }

    let output_stream = quote! {
        impl #struct_name {
            #(#impl_block)*
        }
    };

    output_stream.into()
}

static SUID_COUNTER: AtomicU8 = AtomicU8::new(0);
/// Automatically sets up some boilerplate needed for static subsystems.
/// Expects Subsystem name as an argument.
/// Example: subsystem!(TestSubsystem, 1u8)
#[proc_macro]
pub fn subsystem(input: TokenStream) -> TokenStream {
    //get an ident and a literal int from the token stream
    //filter out puncts and commas
    let mut iter = TokenStream2::from(input).into_iter().filter(|token| {
        !matches!(
            token,
            proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Group(_)
        )
    });
    let struct_name =
        syn::parse2::<syn::Ident>(iter.next().expect("could not find first ident").into())
            .expect("could not parse first ident as an ident");

    //get the struct name in caps as an identifier
    let struct_name_caps = syn::Ident::new(
        &format!("__{}", struct_name.to_string().to_uppercase()),
        struct_name.span(),
    );

    let mut output = TokenStream2::new();

    //turn SUID_COUNT into a literal int
    let count = SUID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let suid_count = syn::LitInt::new(&count.to_string(), proc_macro2::Span::call_site());

    // create a static variable for the struct
    let static_variable = quote! {
        use wpilib::re_exports::parking_lot as p_l;
        use wpilib::re_exports::once_cell as o_c;
        static #struct_name_caps: o_c::sync::Lazy<p_l::Mutex<#struct_name>> = o_c::sync::Lazy::new(|| p_l::Mutex::new(#struct_name::__new()));
        static SUID: u8 = #suid_count;
    };
    output.extend(static_variable);

    //add a static fn to get a &mut self from static variable mutex
    let static_getter = quote!(
        impl #struct_name {
            pub fn get_static() -> p_l::MutexGuard<'static, #struct_name> {
                let mut this = #struct_name_caps.lock();
                this
            }
            pub fn suid() -> u8 {
                SUID as u8
            }
            pub fn name() -> &'static str {
                stringify!(#struct_name)
            }
        }
    );
    output.extend(static_getter);

    output.into()
}

struct ReceiverReplacer;

impl VisitMut for ReceiverReplacer {
    fn visit_ident_mut(&mut self, node: &mut proc_macro2::Ident) {
        let maybe_receiver = syn::parse2::<syn::Receiver>(node.clone().to_token_stream());
        if maybe_receiver.is_ok() {
            *node = syn::Ident::new(&format!("__self"), proc_macro2::Span::call_site());
        }
    }
}
fn get_subsystem_use_structure(input: TokenStream) -> (TokenStream2, TokenStream2, TokenStream2) {
    let mut iter = TokenStream2::from(input).into_iter().filter(|token| {
        matches!(
            token,
            proc_macro2::TokenTree::Ident(_)
                | proc_macro2::TokenTree::Punct(_)
                | proc_macro2::TokenTree::Group(_)
        )
    });

    let mut maybe_ident = iter.next().expect("Could not find identifier for command");

    let mut arc_pointers = Vec::<syn::Ident>::new();
    let mut _self: Option<syn::Receiver> = None;

    while match maybe_ident {
        proc_macro2::TokenTree::Ident(_) => true,
        _ => false,
    } {
        let maybe_punc = iter
            .next()
            .expect("Could not find comma after identifier. Did you forget the body?");
        // assure we have
        if match maybe_punc {
            proc_macro2::TokenTree::Punct(ref punc) => punc.as_char() != ',',
            _ => true,
        } {
            panic!("Could not find comma after identifier.");
        }

        let ident = syn::parse2::<syn::Ident>(maybe_ident.clone().into());
        if ident.is_err() {
            let receiver = syn::parse2::<syn::Receiver>(maybe_ident.into());
            match _self {
                Some(_) => panic!("Did not expect self to be passed in twice"),
                _ => {}
            }
            _self = Some(receiver.expect("could not parse ident or self"));
        } else {
            arc_pointers.push(ident.expect("Could not parse ident"));
        }
        maybe_ident = iter.next().expect("Could not find identifier for command");
    }

    let mut main_block = syn::parse2::<syn::ExprBlock>(maybe_ident.into()).expect("Expected Block");

    let mut new_arc_pointers = Vec::<syn::Ident>::new();
    let mut copy_block = TokenStream2::new();

    for arc_ptr in &arc_pointers {
        let copy_quote = quote!(
            let #arc_ptr  = #arc_ptr.clone();
        );
        copy_block.extend(copy_quote);
        new_arc_pointers.push(arc_ptr.clone());
    }

    if _self.is_some() {
        let _self_unwrapped = _self.unwrap();
        let new_arc_ptr = syn::Ident::new(&format!("__self"), proc_macro2::Span::call_site());
        let copy_quote = quote! {
            let #new_arc_ptr = #_self_unwrapped.clone();
        };
        copy_block.extend(copy_quote);
        new_arc_pointers.push(new_arc_ptr);

        ReceiverReplacer.visit_expr_block_mut(&mut main_block);
    }
    let mut acquire_group = TokenStream2::new();

    for arc_ptr in &new_arc_pointers {
        let copy_quote = quote! {
            let mut #arc_ptr = #arc_ptr.0.lock();
        };
        acquire_group.extend(copy_quote);
    }

    let main_block_stream = main_block.to_token_stream();
    return (copy_block, acquire_group, main_block_stream);
}

#[proc_macro]
pub fn command(input: TokenStream) -> TokenStream {
    let (copy_block, acquire_group, main_block) = get_subsystem_use_structure(input);
    let mut output = TokenStream2::new();
    let closure_open = quote! {
        {
            #copy_block
            move || {
                #acquire_group
                #main_block
            }
        }
    };

    output.extend(closure_open);

    output.into()
}

#[proc_macro]
pub fn command_end(input: TokenStream) -> TokenStream {
    let (copy_block, acquire_group, main_block) = get_subsystem_use_structure(input);
    let mut output = TokenStream2::new();
    let closure_open = quote! {
        {
            #copy_block
            move |interrupted| {
                #acquire_group
                #main_block
            }
        }
    };

    output.extend(closure_open);

    output.into()
}

#[proc_macro]
pub fn command_provider(input: TokenStream) -> TokenStream {
    // TODO: Don't need to generate acquire_group
    let (copy_block, _, main_block) = get_subsystem_use_structure(input);
    let mut output = TokenStream2::new();
    let closure_open = quote! {
        {
            #copy_block
            move || {
                #main_block
            }
        }
    };

    output.extend(closure_open);

    output.into()
}

#[proc_macro]
pub fn use_subsystem(input: TokenStream) -> TokenStream {
    // TODO: Don't need to generate acquire_group
    let (copy_block, acquire_group, main_block) = get_subsystem_use_structure(input);
    let mut output = TokenStream2::new();
    let closure_open = quote! {
        {
            #copy_block
            {
                #acquire_group
                #main_block
            }
        }
    };

    output.extend(closure_open);

    output.into()
}

#[proc_macro]
pub fn unit(input: TokenStream) -> TokenStream {
    let mut output = TokenStream2::new();
    //get an ident and a type from the token stream
    //filter out puncts and commas
    let mut iter = TokenStream2::from(input).into_iter().filter(|token| {
        !matches!(
            token,
            proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Group(_)
        )
    });
    let struct_name =
        syn::parse2::<syn::Ident>(iter.next().expect("could not find first ident").into())
            .expect("could not parse first ident as an ident");
    let r#type = syn::parse2::<syn::Ident>(iter.next().expect("could not find second type").into())
        .expect("could not parse second type");

    //create a new struct with the given name and type
    let struct_item = quote! {
        #[forbid(non_camel_case_types)]
        pub struct #struct_name {
            pub(super) value: #r#type,
        }
    };

    //impl clone, copy, debug and display for the struct
    let impl_basic_block = quote! {
        impl Clone for #struct_name {
            fn clone(&self) -> Self {
                Self {
                    value: self.value.clone(),
                }
            }
        }
        impl Copy for #struct_name {}
        impl std::fmt::Debug for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!(#struct_name), self.value)
            }
        }
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!(#struct_name), self.value)
            }
        }
        impl #struct_name {
            #[inline(always)]
            pub fn new(value: #r#type) -> Self {
                Self { value }
            }
            #[inline(always)]
            pub fn value(&self) -> #r#type {
                self.value
            }
            #[inline(always)]
            pub fn set(&mut self, value: #r#type) {
                self.value = value;
            }
        }
    };

    //implement math operators for the struct
    let impl_math_block = quote! {
        impl std::ops::Add for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn add(self, rhs: Self) -> Self::Output {
                Self {
                    value: self.value + rhs.value,
                }
            }
        }
        impl std::ops::Add<&#struct_name> for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn add(self, rhs: &#struct_name) -> Self::Output {
                Self {
                    value: self.value + rhs.value,
                }
            }
        }
        impl std::ops::Add<#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn add(self, rhs: #struct_name) -> Self::Output {
                #struct_name {
                    value: self.value + rhs.value,
                }
            }
        }
        impl std::ops::Add<&#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn add(self, rhs: &#struct_name) -> Self::Output {
                #struct_name {
                    value: self.value + rhs.value,
                }
            }
        }
        impl std::ops::AddAssign for #struct_name {
            #[inline(always)]
            fn add_assign(&mut self, rhs: Self) {
                self.value += rhs.value;
            }
        }
        impl std::ops::Sub for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn sub(self, rhs: Self) -> Self::Output {
                Self {
                    value: self.value - rhs.value,
                }
            }
        }
        impl std::ops::Sub<&#struct_name> for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn sub(self, rhs: &#struct_name) -> Self::Output {
                Self {
                    value: self.value - rhs.value,
                }
            }
        }
        impl std::ops::Sub<#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn sub(self, rhs: #struct_name) -> Self::Output {
                #struct_name {
                    value: self.value - rhs.value,
                }
            }
        }
        impl std::ops::Sub<&#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn sub(self, rhs: &#struct_name) -> Self::Output {
                #struct_name {
                    value: self.value - rhs.value,
                }
            }
        }
        impl std::ops::SubAssign for #struct_name {
            #[inline(always)]
            fn sub_assign(&mut self, rhs: Self) {
                self.value -= rhs.value;
            }
        }
        impl std::ops::Mul for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn mul(self, rhs: Self) -> Self::Output {
                Self {
                    value: self.value * rhs.value,
                }
            }
        }
        impl std::ops::Mul<&#struct_name> for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn mul(self, rhs: &#struct_name) -> Self::Output {
                Self {
                    value: self.value * rhs.value,
                }
            }
        }
        impl std::ops::Mul<#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn mul(self, rhs: #struct_name) -> Self::Output {
                #struct_name {
                    value: self.value * rhs.value,
                }
            }
        }
        impl std::ops::Mul<&#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn mul(self, rhs: &#struct_name) -> Self::Output {
                #struct_name {
                    value: self.value * rhs.value,
                }
            }
        }
        impl std::ops::MulAssign for #struct_name {
            #[inline(always)]
            fn mul_assign(&mut self, rhs: Self) {
                self.value *= rhs.value;
            }
        }
        impl std::ops::Div for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn div(self, rhs: Self) -> Self::Output {
                Self {
                    value: self.value / rhs.value,
                }
            }
        }
        impl std::ops::Div<&#struct_name> for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn div(self, rhs: &#struct_name) -> Self::Output {
                Self {
                    value: self.value / rhs.value,
                }
            }
        }
        impl std::ops::Div<#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn div(self, rhs: #struct_name) -> Self::Output {
                #struct_name {
                    value: self.value / rhs.value,
                }
            }
        }
        impl std::ops::Div<&#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn div(self, rhs: &#struct_name) -> Self::Output {
                #struct_name {
                    value: self.value / rhs.value,
                }
            }
        }
        impl std::ops::DivAssign for #struct_name {
            #[inline(always)]
            fn div_assign(&mut self, rhs: Self) {
                self.value /= rhs.value;
            }
        }
        impl std::ops::Rem for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn rem(self, rhs: Self) -> Self::Output {
                Self {
                    value: self.value % rhs.value,
                }
            }
        }
        impl std::ops::Rem<&#struct_name> for #struct_name {
            type Output = Self;
            #[inline(always)]
            fn rem(self, rhs: &#struct_name) -> Self::Output {
                Self {
                    value: self.value % rhs.value,
                }
            }
        }
        impl std::ops::Rem<#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn rem(self, rhs: #struct_name) -> Self::Output {
                #struct_name {
                    value: self.value % rhs.value,
                }
            }
        }
        impl std::ops::Rem<&#struct_name> for &#struct_name {
            type Output = #struct_name;
            #[inline(always)]
            fn rem(self, rhs: &#struct_name) -> Self::Output {
                #struct_name {
                    value: self.value % rhs.value,
                }
            }
        }
        impl std::ops::RemAssign for #struct_name {
            #[inline(always)]
            fn rem_assign(&mut self, rhs: Self) {
                self.value %= rhs.value;
            }
        }
        impl #struct_name {
            #[inline(always)]
            pub fn square(&self) -> Self {
                Self {
                    value: self.value * self.value,
                }
            }
            #[inline(always)]
            pub fn cube(&self) -> Self {
                Self {
                    value: self.value * self.value * self.value,
                }
            }
            #[inline(always)]
            pub fn map(&self, f: impl FnOnce(#r#type) -> #r#type) -> Self {
                Self {
                    value: f(self.value),
                }
            }
        }
    };

    //implement num traits for the struct
    let impl_num_traits_block = quote! {
        impl wpilib::re_exports::num::Zero for #struct_name {
            fn zero() -> Self {
                Self {
                    value: #r#type::zero(),
                }
            }
            fn is_zero(&self) -> bool {
                self.value.is_zero()
            }
        }
        impl wpilib::re_exports::num::One for #struct_name {
            fn one() -> Self {
                Self {
                    value: #r#type::one(),
                }
            }
            fn is_one(&self) -> bool {
                self.value.is_one()
            }
        }
        impl wpilib::re_exports::num::Num for #struct_name {
            type FromStrRadixErr = <#r#type as wpilib::re_exports::num::Num>::FromStrRadixErr;
            fn from_str_radix(str: &str, radix: u32) -> Result<Self, Self::FromStrRadixErr> {
                Ok(Self {
                    value: #r#type::from_str_radix(str, radix)?,
                })
            }
        }
        // impl wpilib::re_exports::num::NumCast for #struct_name {
        //     fn from<T: wpilib::re_exports::num::ToPrimitive>(n: T) -> Option<Self> {
        //         Some(Self {
        //             value: #r#type::from(n)?,
        //         })
        //     }
        // }
        impl wpilib::re_exports::num::ToPrimitive for #struct_name {
            fn to_i64(&self) -> Option<i64> {
                self.value.to_i64()
            }
            fn to_u64(&self) -> Option<u64> {
                self.value.to_u64()
            }
        }
        impl wpilib::re_exports::num::FromPrimitive for #struct_name {
            fn from_i64(n: i64) -> Option<Self> {
                Some(Self {
                    value: #r#type::from_i64(n)?,
                })
            }
            fn from_u64(n: u64) -> Option<Self> {
                Some(Self {
                    value: #r#type::from_u64(n)?,
                })
            }
            fn from_f64(n: f64) -> Option<Self> {
                Some(Self {
                    value: #r#type::from_f64(n)?,
                })
            }
        }
    };

    //implement into and from for its type
    let impl_into_from_block = quote! {
        impl From<#struct_name> for #r#type {
            #[inline(always)]
            fn from(value: #struct_name) -> #r#type {
                value.value
            }
        }
        impl From<&#struct_name> for #r#type {
            #[inline(always)]
            fn from(value: &#struct_name) -> #r#type {
                value.value
            }
        }
        impl From<f64> for #struct_name {
            #[inline(always)]
            fn from(value: f64) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&f64> for #struct_name {
            #[inline(always)]
            fn from(value: &f64) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<f32> for #struct_name {
            #[inline(always)]
            fn from(value: f32) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&f32> for #struct_name {
            #[inline(always)]
            fn from(value: &f32) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<u64> for #struct_name {
            #[inline(always)]
            fn from(value: u64) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&u64> for #struct_name {
            #[inline(always)]
            fn from(value: &u64) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<u32> for #struct_name {
            #[inline(always)]
            fn from(value: u32) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&u32> for #struct_name {
            #[inline(always)]
            fn from(value: &u32) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<u16> for #struct_name {
            #[inline(always)]
            fn from(value: u16) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&u16> for #struct_name {
            #[inline(always)]
            fn from(value: &u16) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<u8> for #struct_name {
            #[inline(always)]
            fn from(value: u8) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&u8> for #struct_name {
            #[inline(always)]
            fn from(value: &u8) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<i64> for #struct_name {
            #[inline(always)]
            fn from(value: i64) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&i64> for #struct_name {
            #[inline(always)]
            fn from(value: &i64) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<i32> for #struct_name {
            #[inline(always)]
            fn from(value: i32) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&i32> for #struct_name {
            #[inline(always)]
            fn from(value: &i32) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<i16> for #struct_name {
            #[inline(always)]
            fn from(value: i16) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&i16> for #struct_name {
            #[inline(always)]
            fn from(value: &i16) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
        impl From<i8> for #struct_name {
            #[inline(always)]
            fn from(value: i8) -> Self {
                Self {
                    value: value as #r#type,
                }
            }
        }
        impl From<&i8> for #struct_name {
            #[inline(always)]
            fn from(value: &i8) -> Self {
                Self {
                    value: *value as #r#type,
                }
            }
        }
    };

    //implement serde for the struct
    let impl_serde_block = quote! {
        impl wpilib::re_exports::serde::Serialize for #struct_name {
            fn serialize<S: wpilib::re_exports::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.value.serialize(serializer)
            }
        }
        impl<'de> wpilib::re_exports::serde::Deserialize<'de> for #struct_name {
            fn deserialize<D: wpilib::re_exports::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #r#type::deserialize(deserializer).map(|value| Self { value })
            }
        }
    };

    //implement partial eq and partial ord for the struct
    let impl_partial_eq_block = quote! {
        impl std::cmp::PartialEq for #struct_name {
            fn eq(&self, other: &Self) -> bool {
                self.value == other.value
            }
        }
        impl std::cmp::PartialOrd for #struct_name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                self.value.partial_cmp(&other.value)
            }
        }
    };

    let impl_negative_block = quote! {
        impl std::ops::Neg for #struct_name {
            type Output = Self;
            fn neg(self) -> Self::Output {
                Self {
                    value: -self.value,
                }
            }
        }
    };

    let impl_simd_block = quote! {
        impl wpilib::re_exports::nalgebra::SimdValue for #struct_name {
            type Element = #struct_name;
            type SimdBool = bool;

            #[inline]
            fn lanes() -> usize {
                1
            }
            #[inline]
            fn splat(val: Self::Element) -> Self {
                val
            }
            #[inline]
            fn extract(&self, _: usize) -> Self::Element {
                *self
            }
            #[inline]
            unsafe fn extract_unchecked(&self, _: usize) -> Self::Element {
                *self
            }
            #[inline]
            fn replace(&mut self, _: usize, val: Self::Element) {
                self.value = val.value
            }
            #[inline]
            unsafe fn replace_unchecked(&mut self, _: usize, val: Self::Element) {
                self.value = val.value
            }
            #[inline]
            fn select(self, cond: Self::SimdBool, other: Self) -> Self {
                if cond {
                    self
                } else {
                    other
                }
            }
            #[inline]
            fn map_lanes(self, f: impl Fn(Self::Element) -> Self::Element) -> Self
                where
                    Self: Clone, {
                f(self)
            }
            #[inline]
            fn zip_map_lanes(
                    self,
                    b: Self,
                    f: impl Fn(Self::Element, Self::Element) -> Self::Element,
                ) -> Self
                where
                    Self: Clone, {
                f(self, b)
            }
        }
        impl wpilib::re_exports::nalgebra::Field for #struct_name {}
        impl wpilib::re_exports::simba::scalar::SubsetOf<#struct_name> for #struct_name {
            #[inline]
            fn is_in_subset(_element: &Self) -> bool {true}
            fn to_superset(&self) -> #struct_name {*self}
            fn from_superset(element: &#struct_name) -> Option<Self> {Some(*element)}
            fn from_superset_unchecked(element: &#struct_name) -> Self {*element}
        }
        impl wpilib::re_exports::simba::scalar::SubsetOf<#struct_name> for f64 {
            #[inline]
            fn is_in_subset(_element: &#struct_name) -> bool {true}
            fn to_superset(&self) -> #struct_name {#struct_name::new(*self)}
            fn from_superset(element: &#struct_name) -> Option<Self> {Some(element.value as f64)}
            fn from_superset_unchecked(element: &#struct_name) -> Self {element.value as f64}
        }
        impl wpilib::re_exports::nalgebra::ComplexField for #struct_name {
            type RealField = #r#type;
            #[inline]
            fn is_finite(&self) -> bool {self.value.is_finite()}
            #[inline]
            fn try_sqrt(self) -> Option<Self> {Some(#struct_name::new(self.value.sqrt()))}
            #[inline]
            fn abs(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::abs(#r#type::from(self.value))
            }
            #[inline]
            fn acos(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::acos(#r#type::from(self.value)))
            }
            #[inline]
            fn acosh(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::acosh(#r#type::from(self.value)))
            }
            #[inline]
            fn asin(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::asin(#r#type::from(self.value)))
            }
            #[inline]
            fn asinh(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::asinh(#r#type::from(self.value)))
            }
            #[inline]
            fn atan(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::atan(#r#type::from(self.value)))
            }
            #[inline]
            fn atanh(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::atanh(#r#type::from(self.value)))
            }
            #[inline]
            fn cos(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::cos(#r#type::from(self.value)))
            }
            #[inline]
            fn cosh(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::cosh(#r#type::from(self.value)))
            }
            #[inline]
            fn exp(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::exp(#r#type::from(self.value)))
            }
            #[inline]
            fn ln(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::ln(#r#type::from(self.value)))
            }
            #[inline]
            fn log(self, base: #r#type) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::log(#r#type::from(self.value), base))
            }
            #[inline]
            fn powf(self, n: Self::RealField) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::powf(#r#type::from(self.value), n))
            }
            #[inline]
            fn powi(self, n: i32) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::powi(#r#type::from(self.value), n))
            }
            #[inline]
            fn recip(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::recip(#r#type::from(self.value)))
            }
            #[inline]
            fn sin(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::sin(#r#type::from(self.value)))
            }
            #[inline]
            fn sinh(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::sinh(#r#type::from(self.value)))
            }
            #[inline]
            fn sqrt(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::sqrt(#r#type::from(self.value)))
            }
            #[inline]
            fn tan(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::tan(#r#type::from(self.value)))
            }
            #[inline]
            fn tanh(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::tanh(#r#type::from(self.value)))
            }
            #[inline]
            fn argument(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::argument(#r#type::from(self.value))
            }
            #[inline]
            fn modulus(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::modulus(#r#type::from(self.value))
            }
            #[inline]
            fn to_exp(self) -> (Self::RealField, Self) {
                let (r, theta) = wpilib::re_exports::nalgebra::ComplexField::to_exp(#r#type::from(self.value));
                (r, #struct_name::new(theta))
            }
            #[inline]
            fn cbrt(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::cbrt(#r#type::from(self.value)))
            }
            #[inline]
            fn hypot(self, other: Self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::hypot(#r#type::from(self.value), #r#type::from(other.value))
            }
            #[inline]
            fn ceil(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::ceil(#r#type::from(self.value)))
            }
            #[inline]
            fn floor(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::floor(#r#type::from(self.value)))
            }
            #[inline]
            fn round(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::round(#r#type::from(self.value)))
            }
            #[inline]
            fn trunc(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::trunc(#r#type::from(self.value)))
            }
            #[inline]
            fn conjugate(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::conjugate(#r#type::from(self.value)))
            }
            #[inline]
            fn cosc(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::cosc(#r#type::from(self.value)))
            }
            #[inline]
            fn sinhc(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::sinhc(#r#type::from(self.value)))
            }
            #[inline]
            fn signum(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::signum(#r#type::from(self.value)))
            }
            #[inline]
            fn coshc(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::coshc(#r#type::from(self.value)))
            }
            #[inline]
            fn exp2(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::exp2(#r#type::from(self.value)))
            }
            #[inline]
            fn exp_m1(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::exp_m1(#r#type::from(self.value)))
            }
            #[inline]
            fn ln_1p(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::ln_1p(#r#type::from(self.value)))
            }
            #[inline]
            fn log10(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::log10(#r#type::from(self.value)))
            }
            #[inline]
            fn fract(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::fract(#r#type::from(self.value)))
            }
            #[inline]
            fn from_real(re: Self::RealField) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::from_real(re))
            }
            #[inline]
            fn imaginary(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::imaginary(#r#type::from(self.value))
            }
            #[inline]
            fn log2(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::log2(#r#type::from(self.value)))
            }
            #[inline]
            fn modulus_squared(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::modulus_squared(#r#type::from(self.value))
            }
            #[inline]
            fn mul_add(self,a:Self,b:Self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::mul_add(#r#type::from(self.value),#r#type::from(a.value),#r#type::from(b.value)))
            }
            #[inline]
            fn norm1(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::norm1(#r#type::from(self.value))
            }
            #[inline]
            fn powc(self,n:Self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::powc(#r#type::from(self.value),#r#type::from(n.value)))
            }
            #[inline]
            fn real(self) -> Self::RealField {
                wpilib::re_exports::nalgebra::ComplexField::real(#r#type::from(self.value))
            }
            #[inline]
            fn scale(self,factor:Self::RealField) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::scale(#r#type::from(self.value),factor))
            }
            #[inline]
            fn sin_cos(self) -> (Self,Self) {
                let (s,c) = wpilib::re_exports::nalgebra::ComplexField::sin_cos(#r#type::from(self.value));
                (#struct_name::new(s),#struct_name::new(c))
            }
            #[inline]
            fn sinc(self) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::sinc(#r#type::from(self.value)))
            }
            fn sinh_cosh(self) -> (Self,Self) {
                let (s,c) = wpilib::re_exports::nalgebra::ComplexField::sinh_cosh(#r#type::from(self.value));
                (#struct_name::new(s),#struct_name::new(c))
            }
            fn to_polar(self) -> (Self::RealField,Self::RealField) {
                let (r,theta) = wpilib::re_exports::nalgebra::ComplexField::to_polar(#r#type::from(self.value));
                (r,theta)
            }
            fn unscale(self,factor:Self::RealField) -> Self {
                #struct_name::new(wpilib::re_exports::nalgebra::ComplexField::unscale(#r#type::from(self.value),factor))
            }
        }
    };

    let type_str = r#type.to_string();

    output.extend(struct_item);
    output.extend(impl_basic_block);
    output.extend(impl_math_block);
    output.extend(impl_num_traits_block);
    output.extend(impl_into_from_block);
    output.extend(impl_serde_block);
    output.extend(impl_partial_eq_block);

    if !type_str.contains("u") {
        output.extend(impl_negative_block);
    }
    if type_str.contains("f64") || type_str.contains("f32") {
        output.extend(impl_simd_block);
    }

    output.into()
}

#[proc_macro]
pub fn unit_conversion(input: TokenStream) -> TokenStream {
    let mut output = TokenStream2::new();

    // e.g. wpilib_macros::unit_conversion!(meter f64, Feet f64, meter_to_feet);
    //this would mean meter -> Feet

    let mut iter = TokenStream2::from(input).into_iter().filter(|token| {
        !matches!(
            token,
            proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Group(_)
        )
    });
    let from_name =
        syn::parse2::<syn::Ident>(iter.next().expect("could not find from ident").into())
            .expect("could not parse from ident as an ident");
    let from_inner_type =
        syn::parse2::<syn::Ident>(iter.next().expect("could not find from type ident").into())
            .expect("could not parse from type ident as an ident");
    let to_name = syn::parse2::<syn::Ident>(iter.next().expect("could not find to ident").into())
        .expect("could not parse to ident as an ident");
    let to_inner_type =
        syn::parse2::<syn::Ident>(iter.next().expect("could not find to type ident").into())
            .expect("could not parse to type ident as an ident");
    let conv_func =
        syn::parse2::<syn::Ident>(iter.next().expect("could not find third ident").into())
            .expect("could not parse third ident as an ident");

    let inv_conv_ident = syn::Ident::new(
        &format!("inverse_{}", conv_func),
        proc_macro2::Span::call_site(),
    );

    //create an inverse conv_func
    let inv_conv_func_block = quote! {
        fn #inv_conv_ident(value: #to_inner_type) -> #from_inner_type {
            (value / #conv_func(#from_inner_type::from(1.0)) as #to_inner_type) as #from_inner_type
        }
    };

    let impl_from_block = quote! {
        impl From<#from_name> for #to_name {
            fn from(value: #from_name) -> Self {
                #to_name{ value: #conv_func(value.value)}
            }
        }
        impl From<&#from_name> for #to_name {
            fn from(value: &#from_name) -> Self {
                #to_name{ value: #conv_func(value.value)}
            }
        }
        impl From<#to_name> for #from_name {
            fn from(value: #to_name) -> Self {
                #from_name{ value: #inv_conv_ident(value.value)}
            }
        }
        impl From<&#to_name> for #from_name {
            fn from(value: &#to_name) -> Self {
                #from_name{ value: #inv_conv_ident(value.value)}
            }
        }
    };

    //add math between the two types
    let impl_to_op_from_block = quote! {
        impl std::ops::Add<#to_name> for #from_name {
            type Output = #from_name;
            fn add(self, rhs: #to_name) -> Self::Output {
                self + #from_name::from(rhs)
            }
        }
        impl std::ops::Add<&#to_name> for #from_name {
            type Output = #from_name;
            fn add(self, rhs: &#to_name) -> Self::Output {
                self + #from_name::from(rhs)
            }
        }
        impl std::ops::Add<#to_name> for &#from_name {
            type Output = #from_name;
            fn add(self, rhs: #to_name) -> Self::Output {
                self + #from_name::from(rhs)
            }
        }
        impl std::ops::Add<&#to_name> for &#from_name {
            type Output = #from_name;
            fn add(self, rhs: &#to_name) -> Self::Output {
                self + #from_name::from(rhs)
            }
        }
        impl std::ops::AddAssign<#to_name> for #from_name {
            fn add_assign(&mut self, rhs: #to_name) {
                *self += #from_name::from(rhs);
            }
        }
        impl std::ops::Sub<#to_name> for #from_name {
            type Output = #from_name;
            fn sub(self, rhs: #to_name) -> Self::Output {
                self - #from_name::from(rhs)
            }
        }
        impl std::ops::Sub<&#to_name> for #from_name {
            type Output = #from_name;
            fn sub(self, rhs: &#to_name) -> Self::Output {
                self - #from_name::from(rhs)
            }
        }
        impl std::ops::Sub<#to_name> for &#from_name {
            type Output = #from_name;
            fn sub(self, rhs: #to_name) -> Self::Output {
                self - #from_name::from(rhs)
            }
        }
        impl std::ops::Sub<&#to_name> for &#from_name {
            type Output = #from_name;
            fn sub(self, rhs: &#to_name) -> Self::Output {
                self - #from_name::from(rhs)
            }
        }
        impl std::ops::SubAssign<#to_name> for #from_name {
            fn sub_assign(&mut self, rhs: #to_name) {
                *self -= #from_name::from(rhs);
            }
        }
        impl std::ops::Mul<#to_name> for #from_name {
            type Output = #from_name;
            fn mul(self, rhs: #to_name) -> Self::Output {
                self * #from_name::from(rhs)
            }
        }
        impl std::ops::Mul<&#to_name> for #from_name {
            type Output = #from_name;
            fn mul(self, rhs: &#to_name) -> Self::Output {
                self * #from_name::from(rhs)
            }
        }
        impl std::ops::Mul<#to_name> for &#from_name {
            type Output = #from_name;
            fn mul(self, rhs: #to_name) -> Self::Output {
                self * #from_name::from(rhs)
            }
        }
        impl std::ops::Mul<&#to_name> for &#from_name {
            type Output = #from_name;
            fn mul(self, rhs: &#to_name) -> Self::Output {
                self * #from_name::from(rhs)
            }
        }
        impl std::ops::MulAssign<#to_name> for #from_name {
            fn mul_assign(&mut self, rhs: #to_name) {
                *self *= #from_name::from(rhs);
            }
        }
        impl std::ops::Div<#to_name> for #from_name {
            type Output = #from_name;
            fn div(self, rhs: #to_name) -> Self::Output {
                self / #from_name::from(rhs)
            }
        }
        impl std::ops::Div<&#to_name> for #from_name {
            type Output = #from_name;
            fn div(self, rhs: &#to_name) -> Self::Output {
                self / #from_name::from(rhs)
            }
        }
        impl std::ops::Div<#to_name> for &#from_name {
            type Output = #from_name;
            fn div(self, rhs: #to_name) -> Self::Output {
                self / #from_name::from(rhs)
            }
        }
        impl std::ops::Div<&#to_name> for &#from_name {
            type Output = #from_name;
            fn div(self, rhs: &#to_name) -> Self::Output {
                self / #from_name::from(rhs)
            }
        }
        impl std::ops::DivAssign<#to_name> for #from_name {
            fn div_assign(&mut self, rhs: #to_name) {
                *self /= #from_name::from(rhs);
            }
        }
        impl std::ops::Rem<#to_name> for #from_name {
            type Output = #from_name;
            fn rem(self, rhs: #to_name) -> Self::Output {
                self % #from_name::from(rhs)
            }
        }
        impl std::ops::Rem<&#to_name> for #from_name {
            type Output = #from_name;
            fn rem(self, rhs: &#to_name) -> Self::Output {
                self % #from_name::from(rhs)
            }
        }
        impl std::ops::Rem<#to_name> for &#from_name {
            type Output = #from_name;
            fn rem(self, rhs: #to_name) -> Self::Output {
                self % #from_name::from(rhs)
            }
        }
        impl std::ops::Rem<&#to_name> for &#from_name {
            type Output = #from_name;
            fn rem(self, rhs: &#to_name) -> Self::Output {
                self % #from_name::from(rhs)
            }
        }
        impl std::ops::RemAssign<#to_name> for #from_name {
            fn rem_assign(&mut self, rhs: #to_name) {
                *self %= #from_name::from(rhs);
            }
        }
    };
    let impl_from_op_to_block = quote! {
        impl std::ops::Add<#from_name> for #to_name {
            type Output = #to_name;
            fn add(self, rhs: #from_name) -> Self::Output {
                self + #to_name::from(rhs)
            }
        }
        impl std::ops::Add<&#from_name> for #to_name {
            type Output = #to_name;
            fn add(self, rhs: &#from_name) -> Self::Output {
                self + #to_name::from(rhs)
            }
        }
        impl std::ops::Add<#from_name> for &#to_name {
            type Output = #to_name;
            fn add(self, rhs: #from_name) -> Self::Output {
                self + #to_name::from(rhs)
            }
        }
        impl std::ops::Add<&#from_name> for &#to_name {
            type Output = #to_name;
            fn add(self, rhs: &#from_name) -> Self::Output {
                self + #to_name::from(rhs)
            }
        }
        impl std::ops::AddAssign<#from_name> for #to_name {
            fn add_assign(&mut self, rhs: #from_name) {
                *self += #to_name::from(rhs);
            }
        }
        impl std::ops::Sub<#from_name> for #to_name {
            type Output = #to_name;
            fn sub(self, rhs: #from_name) -> Self::Output {
                self - #to_name::from(rhs)
            }
        }
        impl std::ops::Sub<&#from_name> for #to_name {
            type Output = #to_name;
            fn sub(self, rhs: &#from_name) -> Self::Output {
                self - #to_name::from(rhs)
            }
        }
        impl std::ops::Sub<#from_name> for &#to_name {
            type Output = #to_name;
            fn sub(self, rhs: #from_name) -> Self::Output {
                self - #to_name::from(rhs)
            }
        }
        impl std::ops::Sub<&#from_name> for &#to_name {
            type Output = #to_name;
            fn sub(self, rhs: &#from_name) -> Self::Output {
                self - #to_name::from(rhs)
            }
        }
        impl std::ops::SubAssign<#from_name> for #to_name {
            fn sub_assign(&mut self, rhs: #from_name) {
                *self -= #to_name::from(rhs);
            }
        }
        impl std::ops::Mul<#from_name> for #to_name {
            type Output = #to_name;
            fn mul(self, rhs: #from_name) -> Self::Output {
                self * #to_name::from(rhs)
            }
        }
        impl std::ops::Mul<&#from_name> for #to_name {
            type Output = #to_name;
            fn mul(self, rhs: &#from_name) -> Self::Output {
                self * #to_name::from(rhs)
            }
        }
        impl std::ops::Mul<#from_name> for &#to_name {
            type Output = #to_name;
            fn mul(self, rhs: #from_name) -> Self::Output {
                self * #to_name::from(rhs)
            }
        }
        impl std::ops::Mul<&#from_name> for &#to_name {
            type Output = #to_name;
            fn mul(self, rhs: &#from_name) -> Self::Output {
                self * #to_name::from(rhs)
            }
        }
        impl std::ops::MulAssign<#from_name> for #to_name {
            fn mul_assign(&mut self, rhs: #from_name) {
                *self *= #to_name::from(rhs);
            }
        }
        impl std::ops::Div<#from_name> for #to_name {
            type Output = #to_name;
            fn div(self, rhs: #from_name) -> Self::Output {
                self / #to_name::from(rhs)
            }
        }
        impl std::ops::Div<&#from_name> for #to_name {
            type Output = #to_name;
            fn div(self, rhs: &#from_name) -> Self::Output {
                self / #to_name::from(rhs)
            }
        }
        impl std::ops::Div<#from_name> for &#to_name {
            type Output = #to_name;
            fn div(self, rhs: #from_name) -> Self::Output {
                self / #to_name::from(rhs)
            }
        }
        impl std::ops::Div<&#from_name> for &#to_name {
            type Output = #to_name;
            fn div(self, rhs: &#from_name) -> Self::Output {
                self / #to_name::from(rhs)
            }
        }
        impl std::ops::DivAssign<#from_name> for #to_name {
            fn div_assign(&mut self, rhs: #from_name) {
                *self /= #to_name::from(rhs);
            }
        }
        impl std::ops::Rem<#from_name> for #to_name {
            type Output = #to_name;
            fn rem(self, rhs: #from_name) -> Self::Output {
                self % #to_name::from(rhs)
            }
        }
        impl std::ops::Rem<&#from_name> for #to_name {
            type Output = #to_name;
            fn rem(self, rhs: &#from_name) -> Self::Output {
                self % #to_name::from(rhs)
            }
        }
        impl std::ops::Rem<#from_name> for &#to_name {
            type Output = #to_name;
            fn rem(self, rhs: #from_name) -> Self::Output {
                self % #to_name::from(rhs)
            }
        }
        impl std::ops::Rem<&#from_name> for &#to_name {
            type Output = #to_name;
            fn rem(self, rhs: &#from_name) -> Self::Output {
                self % #to_name::from(rhs)
            }
        }
        impl std::ops::RemAssign<#from_name> for #to_name {
            fn rem_assign(&mut self, rhs: #from_name) {
                *self %= #to_name::from(rhs);
            }
        }
    };

    //implement partial eq and partial ord between the two types
    let impl_partial_eq_ord_block = quote! {
        impl std::cmp::PartialEq<#to_name> for #from_name {
            fn eq(&self, other: &#to_name) -> bool {
                self.value == #inv_conv_ident(other.value) as #from_inner_type
            }
        }
        impl std::cmp::PartialEq<#from_name> for #to_name {
            fn eq(&self, other: &#from_name) -> bool {
                self.value == #conv_func(other.value) as #to_inner_type
            }
        }
        impl std::cmp::PartialOrd<#to_name> for #from_name {
            fn partial_cmp(&self, other: &#to_name) -> Option<std::cmp::Ordering> {
                self.value.partial_cmp(&#inv_conv_ident(other.value))
            }
        }
        impl std::cmp::PartialOrd<#from_name> for #to_name {
            fn partial_cmp(&self, other: &#from_name) -> Option<std::cmp::Ordering> {
                self.value.partial_cmp(&#conv_func(other.value))
            }
        }
    };

    output.extend(inv_conv_func_block);
    output.extend(impl_from_block);
    output.extend(impl_to_op_from_block);
    output.extend(impl_from_op_to_block);
    output.extend(impl_partial_eq_ord_block);

    output.into()
}

#[proc_macro]
pub fn unit_dimensional_analysis(input: TokenStream) -> TokenStream {
    let mut output = TokenStream2::new();
    //expect (ident operator ident = ident)
    let input = TokenStream2::from(input);
    let mut iter = input.into_iter();
    let a_name = iter.next().unwrap();
    let operator = iter.next().unwrap();
    let b_name = iter.next().unwrap();
    let eq = iter.next().unwrap();
    let c_name = iter.next().unwrap();
    let a_name = match a_name {
        proc_macro2::TokenTree::Ident(ident) => ident,
        _ => panic!("expected ident"),
    };
    let b_name = match b_name {
        proc_macro2::TokenTree::Ident(ident) => ident,
        _ => panic!("expected ident"),
    };
    let c_name = match c_name {
        proc_macro2::TokenTree::Ident(ident) => ident,
        _ => panic!("expected ident"),
    };
    let operator = match operator {
        proc_macro2::TokenTree::Punct(ident) => ident,
        _ => panic!("expected punct"),
    };
    let eq = match eq {
        proc_macro2::TokenTree::Punct(ident) => ident,
        _ => panic!("expected punct"),
    };

    if eq.as_char() != '=' {
        panic!("expected =");
    }

    let impl_block;

    match operator.as_char() {
        '*' => {
            impl_block = quote! {
                impl std::ops::Mul<#b_name> for #a_name {
                    type Output = #c_name;
                    fn mul(self, rhs: #b_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<#a_name> for #b_name {
                    type Output = #c_name;
                    fn mul(self, rhs: #a_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<&#b_name> for #a_name {
                    type Output = #c_name;
                    fn mul(self, rhs: &#b_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<#a_name> for &#b_name {
                    type Output = #c_name;
                    fn mul(self, rhs: #a_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<#b_name> for &#a_name {
                    type Output = #c_name;
                    fn mul(self, rhs: #b_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<&#a_name> for #b_name {
                    type Output = #c_name;
                    fn mul(self, rhs: &#a_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<&#b_name> for &#a_name {
                    type Output = #c_name;
                    fn mul(self, rhs: &#b_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }
                impl std::ops::Mul<&#a_name> for &#b_name {
                    type Output = #c_name;
                    fn mul(self, rhs: &#a_name) -> Self::Output {
                        #c_name::from(self.value * rhs.value)
                    }
                }

                //other order
                impl std::ops::Div<#a_name> for #c_name {
                    type Output = #b_name;
                    fn div(self, rhs: #a_name) -> Self::Output {
                        #b_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<#b_name> for #c_name {
                    type Output = #a_name;
                    fn div(self, rhs: #b_name) -> Self::Output {
                        #a_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<&#a_name> for #c_name {
                    type Output = #b_name;
                    fn div(self, rhs: &#a_name) -> Self::Output {
                        #b_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<#a_name> for &#c_name {
                    type Output = #b_name;
                    fn div(self, rhs: #a_name) -> Self::Output {
                        #b_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<&#a_name> for &#c_name {
                    type Output = #b_name;
                    fn div(self, rhs: &#a_name) -> Self::Output {
                        #b_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<&#b_name> for #c_name {
                    type Output = #a_name;
                    fn div(self, rhs: &#b_name) -> Self::Output {
                        #a_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<#b_name> for &#c_name {
                    type Output = #a_name;
                    fn div(self, rhs: #b_name) -> Self::Output {
                        #a_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<&#b_name> for &#c_name {
                    type Output = #a_name;
                    fn div(self, rhs: &#b_name) -> Self::Output {
                        #a_name::from(self.value / rhs.value)
                    }
                }
            };
        }
        '/' => {
            impl_block = quote! {
                impl std::ops::Div<#b_name> for #a_name {
                    type Output = #c_name;
                    fn div(self, rhs: #b_name) -> Self::Output {
                        #c_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<&#b_name> for #a_name {
                    type Output = #c_name;
                    fn div(self, rhs: &#b_name) -> Self::Output {
                        #c_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<#b_name> for &#a_name {
                    type Output = #c_name;
                    fn div(self, rhs: #b_name) -> Self::Output {
                        #c_name::from(self.value / rhs.value)
                    }
                }
                impl std::ops::Div<&#b_name> for &#a_name {
                    type Output = #c_name;
                    fn div(self, rhs: &#b_name) -> Self::Output {
                        #c_name::from(self.value / rhs.value)
                    }
                }
            };
        }
        _ => panic!("expected * /"),
    }

    output.extend(impl_block);

    output.into()
}
