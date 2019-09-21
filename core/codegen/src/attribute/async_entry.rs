use devise::{syn, Diagnostic, Spanned, Result};
use devise::ext::SpanDiagnosticExt;
use devise::proc_macro2::{TokenStream, Span};

trait EntryAttr {
    /// Whether the attribute requires the attributed function to be `async`.
    const REQUIRES_ASYNC: bool;

    /// Return a new or rewritten function, using block as the main execution.
    fn function(f: &syn::ItemFn, body: &syn::Block) -> Result<TokenStream>;
}

struct Main;

impl EntryAttr for Main {
    const REQUIRES_ASYNC: bool = true;

    fn function(f: &syn::ItemFn, block: &syn::Block) -> Result<TokenStream> {
        let (attrs, vis, mut sig) = (&f.attrs, &f.vis, f.sig.clone());
        if sig.ident != "main" {
            return Err(Span::call_site()
                .warning("attribute is typically applied to `main` function")
                .span_note(sig.span(), "this function is not `main`"));
        }

        sig.asyncness = None;
        Ok(quote_spanned!(block.span().into() => #(#attrs)* #vis #sig {
            ::rocket::async_main(async move #block)
        }))
    }
}

struct Test;

impl EntryAttr for Test {
    const REQUIRES_ASYNC: bool = true;

    fn function(f: &syn::ItemFn, block: &syn::Block) -> Result<TokenStream> {
        let (attrs, vis, mut sig) = (&f.attrs, &f.vis, f.sig.clone());
        sig.asyncness = None;
        Ok(quote_spanned!(block.span().into() => #(#attrs)* #[test] #vis #sig {
            ::rocket::async_test(async move #block)
        }))
    }
}

struct Launch;

impl EntryAttr for Launch {
    const REQUIRES_ASYNC: bool = false;

    fn function(f: &syn::ItemFn, block: &syn::Block) -> Result<TokenStream> {
        if f.sig.ident == "main" {
            return Err(Span::call_site()
                .error("attribute cannot be applied to `main` function")
                .note("this attribute generates a `main` function")
                .span_note(f.sig.span(), "this function cannot be `main`"));
        }

        let ty = match &f.sig.output {
            syn::ReturnType::Type(_, ty) => ty,
            _ => return Err(Span::call_site()
                .error("attribute can only be applied to functions that return a value")
                .span_note(f.sig.span(), "this function must return a value"))
        };

        let rocket = quote_spanned!(ty.span().into() => {
            let ___rocket: #ty = #block;
            let ___rocket: ::rocket::Rocket = ___rocket;
            ___rocket
        });

        // FIXME: Don't duplicate the `#block` here!
        let (vis, mut sig) = (&f.vis, f.sig.clone());
        sig.ident = syn::Ident::new("main", sig.ident.span());
        sig.output = syn::ReturnType::Default;
        sig.asyncness = None;

        Ok(quote_spanned!(block.span().into() =>
            #[allow(dead_code)] #f

            #vis #sig {
                ::rocket::async_main(async move { let _ = #rocket.launch().await; })
            }
        ))
    }
}

fn parse_input<A: EntryAttr>(input: proc_macro::TokenStream) -> Result<syn::ItemFn> {
    let function: syn::ItemFn = syn::parse(input)
        .map_err(Diagnostic::from)
        .map_err(|d| d.help("attribute can only be applied to functions"))?;

    if A::REQUIRES_ASYNC && function.sig.asyncness.is_none() {
        return Err(Span::call_site()
            .error("attribute can only be applied to `async` functions")
            .span_note(function.sig.span(), "this function must be `async`"));
    }

    if !function.sig.inputs.is_empty() {
        return Err(Span::call_site()
            .error("attribute can only be applied to functions without arguments")
            .span_note(function.sig.span(), "this function must take no arguments"));
    }

    Ok(function)
}

fn _async_entry<A: EntryAttr>(
    _args: proc_macro::TokenStream,
    input: proc_macro::TokenStream
) -> Result<TokenStream> {
    let function = parse_input::<A>(input)?;
    A::function(&function, &function.block).map(|t| t.into())
}

macro_rules! async_entry {
    ($name:ident, $kind:ty, $default:expr) => (
        pub fn $name(a: proc_macro::TokenStream, i: proc_macro::TokenStream) -> TokenStream {
            _async_entry::<$kind>(a, i).unwrap_or_else(|d| {
                let d = d.emit_as_tokens();
                let default = $default;
                quote!(#d #default)
            })
        }
    )
}

async_entry!(async_test_attribute, Test, quote!());
async_entry!(main_attribute, Main, quote!(fn main() {}));
async_entry!(launch_attribute, Launch, quote!(fn main() {}));
