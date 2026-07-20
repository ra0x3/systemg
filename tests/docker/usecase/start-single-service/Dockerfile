FROM sysg-usecase-base

COPY tests/docker/usecase/start-single-service/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-single-service/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
