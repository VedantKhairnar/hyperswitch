use api_models::payments;
use common_utils::pii;
use masking::Secret;
use serde::{Deserialize, Serialize};

use crate::{
    connector::utils::{
        self, AddressDetailsData, CardData, CardIssuer, PaymentsAuthorizeRequestData,
        PaymentsSyncRequestData, RouterData,
    },
    consts,
    core::errors,
    types::{
        self,
        api::{self, enums as api_enums},
        storage::enums,
        transformers::ForeignFrom,
    },
};

pub struct BankOfAmericaAuthType {
    pub(super) api_key: Secret<String>,
    pub(super) merchant_account: Secret<String>,
    pub(super) api_secret: Secret<String>,
}

impl TryFrom<&types::ConnectorAuthType> for BankOfAmericaAuthType {
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(auth_type: &types::ConnectorAuthType) -> Result<Self, Self::Error> {
        if let types::ConnectorAuthType::SignatureKey {
            api_key,
            key1,
            api_secret,
        } = auth_type
        {
            Ok(Self {
                api_key: api_key.to_owned(),
                merchant_account: key1.to_owned(),
                api_secret: api_secret.to_owned(),
            })
        } else {
            Err(errors::ConnectorError::FailedToObtainAuthType)?
        }
    }
}

pub struct BankOfAmericaRouterData<T> {
    pub amount: String,
    pub router_data: T,
}

impl<T>
    TryFrom<(
        &types::api::CurrencyUnit,
        types::storage::enums::Currency,
        i64,
        T,
    )> for BankOfAmericaRouterData<T>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        (currency_unit, currency, amount, item): (
            &types::api::CurrencyUnit,
            types::storage::enums::Currency,
            i64,
            T,
        ),
    ) -> Result<Self, Self::Error> {
        let amount = utils::get_amount_as_string(currency_unit, amount, currency)?;
        Ok(Self {
            amount,
            router_data: item,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaPaymentsRequest {
    processing_information: ProcessingInformation,
    payment_information: PaymentInformation,
    order_information: OrderInformationWithBill,
    client_reference_information: ClientReferenceInformation,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingInformation {
    capture: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureOptions {
    capture_sequence_number: u32,
    total_capture_count: u32,
}

#[derive(Debug, Serialize)]
pub struct PaymentInformation {
    card: Card,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    number: cards::CardNumber,
    expiration_month: Secret<String>,
    expiration_year: Secret<String>,
    security_code: Secret<String>,
    #[serde(rename = "type")]
    card_type: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderInformationWithBill {
    amount_details: Amount,
    bill_to: BillTo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Amount {
    total_amount: String,
    currency: api_models::enums::Currency,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillTo {
    first_name: Secret<String>,
    last_name: Secret<String>,
    address1: Secret<String>,
    locality: String,
    administrative_area: Secret<String>,
    postal_code: Secret<String>,
    country: api_enums::CountryAlpha2,
    email: pii::Email,
}

// for bankofamerica each item in Billing is mandatory
fn build_bill_to(
    address_details: &payments::Address,
    email: pii::Email,
) -> Result<BillTo, error_stack::Report<errors::ConnectorError>> {
    let address = address_details
        .address
        .as_ref()
        .ok_or_else(utils::missing_field_err("billing.address"))?;
    Ok(BillTo {
        first_name: address.get_first_name()?.to_owned(),
        last_name: address.get_last_name()?.to_owned(),
        address1: address.get_line1()?.to_owned(),
        locality: address.get_city()?.to_owned(),
        administrative_area: address.to_state_code()?,
        postal_code: address.get_zip()?.to_owned(),
        country: address.get_country()?.to_owned(),
        email,
    })
}

impl From<CardIssuer> for String {
    fn from(card_issuer: CardIssuer) -> Self {
        let card_type = match card_issuer {
            CardIssuer::AmericanExpress => "003",
            CardIssuer::Master => "002",
            //"042" is the type code for Masetro Cards(International). For Maestro Cards(UK-Domestic) the mapping should be "024"
            CardIssuer::Maestro => "042",
            CardIssuer::Visa => "001",
            CardIssuer::Discover => "004",
            CardIssuer::DinersClub => "005",
            CardIssuer::CarteBlanche => "006",
            CardIssuer::JCB => "007",
        };
        card_type.to_string()
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientReferenceInformation {
    code: Option<String>,
}

impl TryFrom<&BankOfAmericaRouterData<&types::PaymentsAuthorizeRouterData>>
    for BankOfAmericaPaymentsRequest
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: &BankOfAmericaRouterData<&types::PaymentsAuthorizeRouterData>,
    ) -> Result<Self, Self::Error> {
        match item.router_data.request.payment_method_data.clone() {
            api::PaymentMethodData::Card(ccard) => {
                let email = item.router_data.request.get_email()?;
                let bill_to = build_bill_to(item.router_data.get_billing()?, email)?;

                let order_information = OrderInformationWithBill {
                    amount_details: Amount {
                        total_amount: item.amount.to_owned(),
                        currency: item.router_data.request.currency,
                    },
                    bill_to,
                };
                let card_issuer = ccard.get_card_issuer();
                let card_type = match card_issuer {
                    Ok(issuer) => Some(String::from(issuer)),
                    Err(_) => None,
                };
                let payment_information = PaymentInformation {
                    card: Card {
                        number: ccard.card_number,
                        expiration_month: ccard.card_exp_month,
                        expiration_year: ccard.card_exp_year,
                        security_code: ccard.card_cvc,
                        card_type,
                    },
                };

                let processing_information = ProcessingInformation {
                    capture: matches!(
                        item.router_data.request.capture_method,
                        Some(enums::CaptureMethod::Automatic) | None
                    ),
                };

                let client_reference_information = ClientReferenceInformation {
                    code: Some(item.router_data.connector_request_reference_id.clone()),
                };

                Ok(Self {
                    processing_information,
                    payment_information,
                    order_information,
                    client_reference_information,
                })
            }
            payments::PaymentMethodData::CardRedirect(_)
            | payments::PaymentMethodData::Wallet(_)
            | payments::PaymentMethodData::PayLater(_)
            | payments::PaymentMethodData::BankRedirect(_)
            | payments::PaymentMethodData::BankDebit(_)
            | payments::PaymentMethodData::BankTransfer(_)
            | payments::PaymentMethodData::Crypto(_)
            | payments::PaymentMethodData::MandatePayment
            | payments::PaymentMethodData::Reward
            | payments::PaymentMethodData::Upi(_)
            | payments::PaymentMethodData::Voucher(_)
            | payments::PaymentMethodData::GiftCard(_) => {
                Err(errors::ConnectorError::NotImplemented(
                    utils::get_unimplemented_payment_method_error_message("Bank of America"),
                )
                .into())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BankofamericaPaymentStatus {
    Authorized,
    Succeeded,
    Failed,
    Voided,
    Reversed,
    Pending,
    Declined,
    AuthorizedPendingReview,
    Transmitted,
}

impl ForeignFrom<(BankofamericaPaymentStatus, bool)> for enums::AttemptStatus {
    fn foreign_from((status, auto_capture): (BankofamericaPaymentStatus, bool)) -> Self {
        match status {
            BankofamericaPaymentStatus::Authorized
            | BankofamericaPaymentStatus::AuthorizedPendingReview => {
                if auto_capture {
                    // Because BankOfAmerica will return Payment Status as Authorized even in AutoCapture Payment
                    Self::Pending
                } else {
                    Self::Authorized
                }
            }
            BankofamericaPaymentStatus::Succeeded | BankofamericaPaymentStatus::Transmitted => {
                Self::Charged
            }
            BankofamericaPaymentStatus::Voided | BankofamericaPaymentStatus::Reversed => {
                Self::Voided
            }
            BankofamericaPaymentStatus::Failed | BankofamericaPaymentStatus::Declined => {
                Self::Failure
            }
            BankofamericaPaymentStatus::Pending => Self::Pending,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BankOfAmericaPaymentsResponse {
    ClientReferenceInformation(BankOfAmericaClientReferenceResponse),
    ErrorInformation(BankOfAmericaErrorInformationResponse),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaClientReferenceResponse {
    id: String,
    status: BankofamericaPaymentStatus,
    client_reference_information: ClientReferenceInformation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaErrorInformationResponse {
    id: String,
    error_information: BankOfAmericaErrorInformation,
}

#[derive(Debug, Deserialize)]
pub struct BankOfAmericaErrorInformation {
    reason: Option<String>,
    message: Option<String>,
}

impl<F>
    TryFrom<
        types::ResponseRouterData<
            F,
            BankOfAmericaPaymentsResponse,
            types::PaymentsAuthorizeData,
            types::PaymentsResponseData,
        >,
    > for types::RouterData<F, types::PaymentsAuthorizeData, types::PaymentsResponseData>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: types::ResponseRouterData<
            F,
            BankOfAmericaPaymentsResponse,
            types::PaymentsAuthorizeData,
            types::PaymentsResponseData,
        >,
    ) -> Result<Self, Self::Error> {
        match item.response {
            BankOfAmericaPaymentsResponse::ClientReferenceInformation(info_response) => Ok(Self {
                status: enums::AttemptStatus::foreign_from((
                    info_response.status,
                    item.data.request.is_auto_capture()?,
                )),
                response: Ok(types::PaymentsResponseData::TransactionResponse {
                    resource_id: types::ResponseId::ConnectorTransactionId(
                        info_response.id.clone(),
                    ),
                    redirection_data: None,
                    mandate_reference: None,
                    connector_metadata: None,
                    network_txn_id: None,
                    connector_response_reference_id: Some(
                        info_response
                            .client_reference_information
                            .code
                            .unwrap_or(info_response.id),
                    ),
                }),
                ..item.data
            }),
            BankOfAmericaPaymentsResponse::ErrorInformation(error_response) => Ok(Self {
                response: Err(types::ErrorResponse {
                    code: consts::NO_ERROR_CODE.to_string(),
                    message: error_response
                        .error_information
                        .message
                        .unwrap_or(consts::NO_ERROR_MESSAGE.to_string()),
                    reason: error_response.error_information.reason,
                    status_code: item.http_code,
                    attempt_status: None,
                }),
                ..item.data
            }),
        }
    }
}

impl<F>
    TryFrom<
        types::ResponseRouterData<
            F,
            BankOfAmericaPaymentsResponse,
            types::PaymentsCaptureData,
            types::PaymentsResponseData,
        >,
    > for types::RouterData<F, types::PaymentsCaptureData, types::PaymentsResponseData>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: types::ResponseRouterData<
            F,
            BankOfAmericaPaymentsResponse,
            types::PaymentsCaptureData,
            types::PaymentsResponseData,
        >,
    ) -> Result<Self, Self::Error> {
        match item.response {
            BankOfAmericaPaymentsResponse::ClientReferenceInformation(info_response) => Ok(Self {
                status: enums::AttemptStatus::foreign_from((info_response.status, true)),
                response: Ok(types::PaymentsResponseData::TransactionResponse {
                    resource_id: types::ResponseId::ConnectorTransactionId(
                        info_response.id.clone(),
                    ),
                    redirection_data: None,
                    mandate_reference: None,
                    connector_metadata: None,
                    network_txn_id: None,
                    connector_response_reference_id: Some(
                        info_response
                            .client_reference_information
                            .code
                            .unwrap_or(info_response.id),
                    ),
                }),
                ..item.data
            }),
            BankOfAmericaPaymentsResponse::ErrorInformation(error_response) => Ok(Self {
                response: Err(types::ErrorResponse {
                    code: consts::NO_ERROR_CODE.to_string(),
                    message: error_response
                        .error_information
                        .message
                        .unwrap_or(consts::NO_ERROR_MESSAGE.to_string()),
                    reason: error_response.error_information.reason,
                    status_code: item.http_code,
                    attempt_status: None,
                }),
                ..item.data
            }),
        }
    }
}

impl<F>
    TryFrom<
        types::ResponseRouterData<
            F,
            BankOfAmericaPaymentsResponse,
            types::PaymentsCancelData,
            types::PaymentsResponseData,
        >,
    > for types::RouterData<F, types::PaymentsCancelData, types::PaymentsResponseData>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: types::ResponseRouterData<
            F,
            BankOfAmericaPaymentsResponse,
            types::PaymentsCancelData,
            types::PaymentsResponseData,
        >,
    ) -> Result<Self, Self::Error> {
        match item.response {
            BankOfAmericaPaymentsResponse::ClientReferenceInformation(info_response) => Ok(Self {
                status: enums::AttemptStatus::foreign_from((info_response.status, false)),
                response: Ok(types::PaymentsResponseData::TransactionResponse {
                    resource_id: types::ResponseId::ConnectorTransactionId(
                        info_response.id.clone(),
                    ),
                    redirection_data: None,
                    mandate_reference: None,
                    connector_metadata: None,
                    network_txn_id: None,
                    connector_response_reference_id: Some(
                        info_response
                            .client_reference_information
                            .code
                            .unwrap_or(info_response.id),
                    ),
                }),
                ..item.data
            }),
            BankOfAmericaPaymentsResponse::ErrorInformation(error_response) => Ok(Self {
                response: Err(types::ErrorResponse {
                    code: consts::NO_ERROR_CODE.to_string(),
                    message: error_response
                        .error_information
                        .message
                        .unwrap_or(consts::NO_ERROR_MESSAGE.to_string()),
                    reason: error_response.error_information.reason,
                    status_code: item.http_code,
                    attempt_status: None,
                }),
                ..item.data
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BankOfAmericaTransactionResponse {
    ApplicationInformation(BankOfAmericaApplicationInfoResponse),
    ErrorInformation(BankOfAmericaErrorInformationResponse),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaApplicationInfoResponse {
    id: String,
    application_information: ApplicationInformation,
    client_reference_information: Option<ClientReferenceInformation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationInformation {
    status: BankofamericaPaymentStatus,
}

impl<F>
    TryFrom<
        types::ResponseRouterData<
            F,
            BankOfAmericaTransactionResponse,
            types::PaymentsSyncData,
            types::PaymentsResponseData,
        >,
    > for types::RouterData<F, types::PaymentsSyncData, types::PaymentsResponseData>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: types::ResponseRouterData<
            F,
            BankOfAmericaTransactionResponse,
            types::PaymentsSyncData,
            types::PaymentsResponseData,
        >,
    ) -> Result<Self, Self::Error> {
        match item.response {
            BankOfAmericaTransactionResponse::ApplicationInformation(app_response) => Ok(Self {
                status: enums::AttemptStatus::foreign_from((
                    app_response.application_information.status,
                    item.data.request.is_auto_capture()?,
                )),
                response: Ok(types::PaymentsResponseData::TransactionResponse {
                    resource_id: types::ResponseId::ConnectorTransactionId(app_response.id.clone()),
                    redirection_data: None,
                    mandate_reference: None,
                    connector_metadata: None,
                    network_txn_id: None,
                    connector_response_reference_id: app_response
                        .client_reference_information
                        .map(|cref| cref.code)
                        .unwrap_or(Some(app_response.id)),
                }),
                ..item.data
            }),
            BankOfAmericaTransactionResponse::ErrorInformation(error_response) => Ok(Self {
                status: item.data.status,
                response: Ok(types::PaymentsResponseData::TransactionResponse {
                    resource_id: types::ResponseId::ConnectorTransactionId(
                        error_response.id.clone(),
                    ),
                    redirection_data: None,
                    mandate_reference: None,
                    connector_metadata: None,
                    network_txn_id: None,
                    connector_response_reference_id: Some(error_response.id),
                }),
                ..item.data
            }),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderInformation {
    amount_details: Amount,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaCaptureRequest {
    order_information: OrderInformation,
    client_reference_information: ClientReferenceInformation,
}

impl TryFrom<&BankOfAmericaRouterData<&types::PaymentsCaptureRouterData>>
    for BankOfAmericaCaptureRequest
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        value: &BankOfAmericaRouterData<&types::PaymentsCaptureRouterData>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            order_information: OrderInformation {
                amount_details: Amount {
                    total_amount: value.amount.to_owned(),
                    currency: value.router_data.request.currency,
                },
            },
            client_reference_information: ClientReferenceInformation {
                code: Some(value.router_data.connector_request_reference_id.clone()),
            },
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaVoidRequest {
    client_reference_information: ClientReferenceInformation,
    reversal_information: ReversalInformation,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReversalInformation {
    amount_details: Amount,
    reason: String,
}

impl TryFrom<&BankOfAmericaRouterData<&types::PaymentsCancelRouterData>>
    for BankOfAmericaVoidRequest
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        value: &BankOfAmericaRouterData<&types::PaymentsCancelRouterData>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            client_reference_information: ClientReferenceInformation {
                code: Some(value.router_data.connector_request_reference_id.clone()),
            },
            reversal_information: ReversalInformation {
                amount_details: Amount {
                    total_amount: value.amount.to_owned(),
                    currency: value.router_data.request.currency.ok_or(
                        errors::ConnectorError::MissingRequiredField {
                            field_name: "Currency",
                        },
                    )?,
                },
                reason: value
                    .router_data
                    .request
                    .cancellation_reason
                    .clone()
                    .ok_or(errors::ConnectorError::MissingRequiredField {
                        field_name: "Cancellation Reason",
                    })?,
            },
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaRefundRequest {
    order_information: OrderInformation,
    client_reference_information: ClientReferenceInformation,
}

impl<F> TryFrom<&BankOfAmericaRouterData<&types::RefundsRouterData<F>>>
    for BankOfAmericaRefundRequest
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: &BankOfAmericaRouterData<&types::RefundsRouterData<F>>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            order_information: OrderInformation {
                amount_details: Amount {
                    total_amount: item.amount.clone(),
                    currency: item.router_data.request.currency,
                },
            },
            client_reference_information: ClientReferenceInformation {
                code: Some(item.router_data.request.refund_id.clone()),
            },
        })
    }
}

impl From<BankofamericaRefundStatus> for enums::RefundStatus {
    fn from(item: BankofamericaRefundStatus) -> Self {
        match item {
            BankofamericaRefundStatus::Succeeded | BankofamericaRefundStatus::Transmitted => {
                Self::Success
            }
            BankofamericaRefundStatus::Failed => Self::Failure,
            BankofamericaRefundStatus::Pending => Self::Pending,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaRefundResponse {
    id: String,
    status: BankofamericaRefundStatus,
}

impl TryFrom<types::RefundsResponseRouterData<api::Execute, BankOfAmericaRefundResponse>>
    for types::RefundsRouterData<api::Execute>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: types::RefundsResponseRouterData<api::Execute, BankOfAmericaRefundResponse>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            response: Ok(types::RefundsResponseData {
                connector_refund_id: item.response.id,
                refund_status: enums::RefundStatus::from(item.response.status),
            }),
            ..item.data
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BankofamericaRefundStatus {
    Succeeded,
    Transmitted,
    Failed,
    Pending,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RsyncApplicationInformation {
    status: BankofamericaRefundStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaRsyncResponse {
    id: String,
    application_information: RsyncApplicationInformation,
}

impl TryFrom<types::RefundsResponseRouterData<api::RSync, BankOfAmericaRsyncResponse>>
    for types::RefundsRouterData<api::RSync>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: types::RefundsResponseRouterData<api::RSync, BankOfAmericaRsyncResponse>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            response: Ok(types::RefundsResponseData {
                connector_refund_id: item.response.id,
                refund_status: enums::RefundStatus::from(
                    item.response.application_information.status,
                ),
            }),
            ..item.data
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankOfAmericaErrorResponse {
    pub error_information: Option<ErrorInformation>,
    pub status: Option<String>,
    pub message: Option<String>,
    pub reason: Option<Reason>,
    pub details: Option<Vec<Details>>,
}

#[derive(Debug, Deserialize, strum::Display)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Reason {
    MissingField,
    InvalidData,
    DuplicateRequest,
    InvalidCard,
    AuthAlreadyReversed,
    CardTypeNotAccepted,
    InvalidMerchantConfiguration,
    ProcessorUnavailable,
    InvalidAmount,
    InvalidCardType,
    InvalidPaymentId,
    NotSupported,
    SystemError,
    ServerTimeout,
    ServiceTimeout,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    pub field: String,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ErrorInformation {
    pub message: String,
    pub reason: String,
}
