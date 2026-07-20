FROM sysg-usecase-base

COPY tests/docker/usecase/start-multi-service-dep-order/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-multi-service-dep-order/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
